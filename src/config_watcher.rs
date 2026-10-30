use crate::{
    config::{ClientConfig, ClientServiceConfig, ServerConfig, ServerServiceConfig},
    Config,
};
use anyhow::{anyhow, Context, Result};
use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
};
use tokio::sync::{broadcast, mpsc};
#[cfg(feature = "notify")]
use tokio::time;
use tracing::{error, info, instrument};

#[cfg(feature = "notify")]
use notify::{EventKind, RecursiveMode, Watcher};

/// Wait after a change before the config is read again
#[cfg(feature = "notify")]
const RELOAD_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ConfigChange {
    General(Box<Config>), // Trigger a full restart
    ServerChange(ServerServiceChange),
    ClientChange(ClientServiceChange),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ClientServiceChange {
    Add(ClientServiceConfig),
    Delete(String),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ServerServiceChange {
    Add(ServerServiceConfig),
    Delete(String),
}

trait InstanceConfig: Clone {
    type ServiceConfig: PartialEq + Eq + Clone;
    fn equal_without_service(&self, rhs: &Self) -> bool;
    fn service_delete_change(s: String) -> ConfigChange;
    fn service_add_change(cfg: Self::ServiceConfig) -> ConfigChange;
    fn get_services(&self) -> &HashMap<String, Self::ServiceConfig>;
}

impl InstanceConfig for ServerConfig {
    type ServiceConfig = ServerServiceConfig;
    fn equal_without_service(&self, rhs: &Self) -> bool {
        let left = ServerConfig {
            services: Default::default(),
            ..self.clone()
        };

        let right = ServerConfig {
            services: Default::default(),
            ..rhs.clone()
        };

        left == right
    }
    fn service_delete_change(s: String) -> ConfigChange {
        ConfigChange::ServerChange(ServerServiceChange::Delete(s))
    }
    fn service_add_change(cfg: Self::ServiceConfig) -> ConfigChange {
        ConfigChange::ServerChange(ServerServiceChange::Add(cfg))
    }
    fn get_services(&self) -> &HashMap<String, Self::ServiceConfig> {
        &self.services
    }
}

impl InstanceConfig for ClientConfig {
    type ServiceConfig = ClientServiceConfig;
    fn equal_without_service(&self, rhs: &Self) -> bool {
        let left = ClientConfig {
            services: Default::default(),
            ..self.clone()
        };

        let right = ClientConfig {
            services: Default::default(),
            ..rhs.clone()
        };

        left == right
    }
    fn service_delete_change(s: String) -> ConfigChange {
        ConfigChange::ClientChange(ClientServiceChange::Delete(s))
    }
    fn service_add_change(cfg: Self::ServiceConfig) -> ConfigChange {
        ConfigChange::ClientChange(ClientServiceChange::Add(cfg))
    }
    fn get_services(&self) -> &HashMap<String, Self::ServiceConfig> {
        &self.services
    }
}

pub struct ConfigWatcherHandle {
    pub event_rx: mpsc::UnboundedReceiver<ConfigChange>,
}

impl ConfigWatcherHandle {
    pub async fn new(path: &Path, shutdown_rx: broadcast::Receiver<bool>) -> Result<Self> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let origin_cfg = Config::from_file(path).await?;

        // Initial start
        event_tx
            .send(ConfigChange::General(Box::new(origin_cfg.clone())))
            .map_err(|_| anyhow!("Failed to queue the initial config"))?;

        tokio::spawn(config_watcher(
            path.to_owned(),
            shutdown_rx,
            event_tx,
            origin_cfg,
        ));

        Ok(ConfigWatcherHandle { event_rx })
    }
}

// Fake config watcher when compiling without `notify`
#[cfg(not(feature = "notify"))]
async fn config_watcher(
    _path: PathBuf,
    mut shutdown_rx: broadcast::Receiver<bool>,
    _event_tx: mpsc::UnboundedSender<ConfigChange>,
    _old: Config,
) -> Result<()> {
    // Do nothing except waiting for ctrl-c
    let _ = shutdown_rx.recv().await;
    Ok(())
}

#[cfg(feature = "notify")]
#[instrument(skip(shutdown_rx, event_tx, old))]
async fn config_watcher(
    path: PathBuf,
    mut shutdown_rx: broadcast::Receiver<bool>,
    event_tx: mpsc::UnboundedSender<ConfigChange>,
    mut old: Config,
) -> Result<()> {
    let (fevent_tx, mut fevent_rx) = mpsc::unbounded_channel();
    let path = if path.is_absolute() {
        path
    } else {
        env::current_dir()?.join(path)
    };
    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, _>| match res {
            // Any change in the watched directories may have replaced the
            // config, the content decides below. Editors, Ansible and `sed -i`
            // write a new file and rename it over the old one, Kubernetes swaps
            // a symlink.
            Ok(e) => {
                if !matches!(e.kind, EventKind::Access(_)) {
                    let _ = fevent_tx.send(());
                }
            }
            Err(e) => error!("watch error: {:#}", e),
        })?;

    for dir in watch_dirs(&path)? {
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;
    }
    info!("Start watching the config");

    loop {
        tokio::select! {
          e = fevent_rx.recv() => {
            if e.is_none() {
                break;
            }
            // A save often comes as several events, read the file once they settled
            time::sleep(RELOAD_DELAY).await;
            while fevent_rx.try_recv().is_ok() {}
            // Removed for a moment while an editor saves, the next event follows
            if !path.exists() {
                continue;
            }

            let new = match Config::from_file(&path).await.with_context(|| "The changed configuration is invalid. Ignored") {
              Ok(v) => v,
              Err(e) => {
                error!("{:#}", e);
                // If the config is invalid, just ignore it
                continue;
              }
            };
            if new == old {
                continue;
            }

            info!("The configuration changed");
            let events = calculate_events(&old, &new).into_iter().flatten();
            for event in events {
                event_tx.send(event)?;
            }

            old = new;
          },
          _ = shutdown_rx.recv() => break
        }
    }

    info!("Config watcher exiting");

    Ok(())
}

/// The directory of the config, and if the config is a symlink also the
/// directory of the file it points to
#[cfg(feature = "notify")]
fn watch_dirs(path: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = vec![path
        .parent()
        .with_context(|| "config file should have a parent dir")?
        .to_path_buf()];
    if let Some(dir) = std::fs::canonicalize(path)
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        if !dirs
            .iter()
            .any(|d| std::fs::canonicalize(d).ok().as_ref() == Some(&dir))
        {
            dirs.push(dir);
        }
    }
    Ok(dirs)
}

fn calculate_events(old: &Config, new: &Config) -> Option<Vec<ConfigChange>> {
    if old == new {
        return None;
    }

    if (old.server.is_some() != new.server.is_some())
        || (old.client.is_some() != new.client.is_some())
    {
        return Some(vec![ConfigChange::General(Box::new(new.clone()))]);
    }

    let mut ret = vec![];

    if let (Some(old_server), Some(new_server)) = (&old.server, &new.server) {
        if old_server != new_server {
            match calculate_instance_config_events(old_server, new_server) {
                Some(mut v) => ret.append(&mut v),
                None => return Some(vec![ConfigChange::General(Box::new(new.clone()))]),
            }
        }
    }

    if let (Some(old_client), Some(new_client)) = (&old.client, &new.client) {
        if old_client != new_client {
            match calculate_instance_config_events(old_client, new_client) {
                Some(mut v) => ret.append(&mut v),
                None => return Some(vec![ConfigChange::General(Box::new(new.clone()))]),
            }
        }
    }

    Some(ret)
}

// None indicates a General change needed
fn calculate_instance_config_events<T: InstanceConfig>(
    old: &T,
    new: &T,
) -> Option<Vec<ConfigChange>> {
    if !old.equal_without_service(new) {
        return None;
    }

    let old = old.get_services();
    let new = new.get_services();

    let deletions = old
        .keys()
        .filter(|&name| new.get(name).is_none())
        .map(|x| T::service_delete_change(x.to_owned()));

    let addition = new
        .iter()
        .filter(|(name, c)| old.get(*name) != Some(*c))
        .map(|(_, c)| T::service_add_change(c.clone()));

    Some(deletions.chain(addition).collect())
}

#[cfg(test)]
mod test {
    use crate::config::ServerConfig;

    use super::*;

    // macro to create map or set literal
    macro_rules! collection {
        // map-like
        ($($k:expr => $v:expr),* $(,)?) => {{
            use std::iter::{Iterator, IntoIterator};
            Iterator::collect(IntoIterator::into_iter([$(($k, $v),)*]))
        }};
    }

    #[test]
    fn test_calculate_events() {
        struct Test {
            old: Config,
            new: Config,
        }

        let tests = [
            Test {
                old: Config {
                    server: Some(Default::default()),
                    client: None,
                },
                new: Config {
                    server: Some(Default::default()),
                    client: Some(Default::default()),
                },
            },
            Test {
                old: Config {
                    server: Some(ServerConfig {
                        bind_addr: String::from("127.0.0.1:2334"),
                        ..Default::default()
                    }),
                    client: None,
                },
                new: Config {
                    server: Some(ServerConfig {
                        bind_addr: String::from("127.0.0.1:2333"),
                        services: collection!(String::from("foo") => Default::default()),
                        ..Default::default()
                    }),
                    client: None,
                },
            },
            Test {
                old: Config {
                    server: Some(Default::default()),
                    client: None,
                },
                new: Config {
                    server: Some(ServerConfig {
                        services: collection!(String::from("foo") => Default::default()),
                        ..Default::default()
                    }),
                    client: None,
                },
            },
            Test {
                old: Config {
                    server: Some(ServerConfig {
                        services: collection!(String::from("foo") => Default::default()),
                        ..Default::default()
                    }),
                    client: None,
                },
                new: Config {
                    server: Some(Default::default()),
                    client: None,
                },
            },
            Test {
                old: Config {
                    server: Some(ServerConfig {
                        services: collection!(String::from("foo1") => ServerServiceConfig::with_name("foo1"), String::from("foo2") => ServerServiceConfig::with_name("foo2")),
                        ..Default::default()
                    }),
                    client: Some(ClientConfig {
                        services: collection!(String::from("foo1") => ClientServiceConfig::with_name("foo1"), String::from("foo2") => ClientServiceConfig::with_name("foo2")),
                        ..Default::default()
                    }),
                },
                new: Config {
                    server: Some(ServerConfig {
                        services: collection!(String::from("bar1") => ServerServiceConfig::with_name("bar1"), String::from("foo2") => ServerServiceConfig::with_name("foo2")),
                        ..Default::default()
                    }),
                    client: Some(ClientConfig {
                        services: collection!(String::from("bar1") => ClientServiceConfig::with_name("bar1"), String::from("bar2") => ClientServiceConfig::with_name("bar2")),
                        ..Default::default()
                    }),
                },
            },
        ];

        let mut expected = [
            vec![ConfigChange::General(Box::new(tests[0].new.clone()))],
            vec![ConfigChange::General(Box::new(tests[1].new.clone()))],
            vec![ConfigChange::ServerChange(ServerServiceChange::Add(
                Default::default(),
            ))],
            vec![ConfigChange::ServerChange(ServerServiceChange::Delete(
                String::from("foo"),
            ))],
            vec![
                ConfigChange::ServerChange(ServerServiceChange::Delete(String::from("foo1"))),
                ConfigChange::ServerChange(ServerServiceChange::Add(
                    tests[4].new.server.as_ref().unwrap().services["bar1"].clone(),
                )),
                ConfigChange::ClientChange(ClientServiceChange::Delete(String::from("foo1"))),
                ConfigChange::ClientChange(ClientServiceChange::Delete(String::from("foo2"))),
                ConfigChange::ClientChange(ClientServiceChange::Add(
                    tests[4].new.client.as_ref().unwrap().services["bar1"].clone(),
                )),
                ConfigChange::ClientChange(ClientServiceChange::Add(
                    tests[4].new.client.as_ref().unwrap().services["bar2"].clone(),
                )),
            ],
        ];

        assert_eq!(tests.len(), expected.len());

        for i in 0..tests.len() {
            let mut actual = calculate_events(&tests[i].old, &tests[i].new).unwrap();

            let get_key = |x: &ConfigChange| -> String {
                match x {
                    ConfigChange::General(_) => String::from("g"),
                    ConfigChange::ServerChange(sc) => match sc {
                        ServerServiceChange::Add(c) => "s_add_".to_owned() + &c.name,
                        ServerServiceChange::Delete(s) => "s_del_".to_owned() + s,
                    },
                    ConfigChange::ClientChange(sc) => match sc {
                        ClientServiceChange::Add(c) => "c_add_".to_owned() + &c.name,
                        ClientServiceChange::Delete(s) => "c_del_".to_owned() + s,
                    },
                }
            };

            actual.sort_by_cached_key(get_key);
            expected[i].sort_by_cached_key(get_key);

            assert_eq!(actual, expected[i]);
        }

        // No changes
        assert_eq!(
            calculate_events(
                &Config {
                    server: Default::default(),
                    client: None,
                },
                &Config {
                    server: Default::default(),
                    client: None,
                },
            ),
            None
        );
    }

    #[cfg(feature = "notify")]
    mod reload {
        use super::*;
        use std::time::Duration;

        const SERVER: &str = "[server]\nbind_addr = \"127.0.0.1:2333\"\n[server.services]\n";
        const SERVICE: &str = "[server.services.foo]\ntoken = \"0123456789abcdef0123\"\nbind_addr = \"127.0.0.1:2334\"\n";

        fn temp_dir(name: &str) -> PathBuf {
            let dir = env::temp_dir().join(format!("rathole-{}-{}", name, std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        async fn next_change(handle: &mut ConfigWatcherHandle) -> ConfigChange {
            tokio::time::timeout(Duration::from_secs(5), handle.event_rx.recv())
                .await
                .expect("no reload within 5s")
                .unwrap()
        }

        async fn expect_added(handle: &mut ConfigWatcherHandle) {
            match next_change(handle).await {
                ConfigChange::ServerChange(ServerServiceChange::Add(s)) => {
                    assert_eq!(s.name, "foo")
                }
                c => panic!("unexpected change {:?}", c),
            }
        }

        // Editors, Ansible and `sed -i` write a new file and rename it over the config
        #[tokio::test]
        async fn replaced_by_rename() {
            let dir = temp_dir("rename");
            let path = dir.join("config.toml");
            std::fs::write(&path, SERVER).unwrap();
            let (_tx, rx) = broadcast::channel(1);
            let mut handle = ConfigWatcherHandle::new(&path, rx).await.unwrap();
            assert!(matches!(
                next_change(&mut handle).await,
                ConfigChange::General(_)
            ));
            // The watcher starts in its own task
            tokio::time::sleep(Duration::from_millis(500)).await;

            let tmp = dir.join(".config.toml.tmp");
            std::fs::write(&tmp, format!("{}{}", SERVER, SERVICE)).unwrap();
            std::fs::rename(&tmp, &path).unwrap();
            expect_added(&mut handle).await;
            std::fs::remove_dir_all(&dir).unwrap();
        }

        // The config is a symlink to a file in another directory
        #[cfg(unix)]
        #[tokio::test]
        async fn symlink_target_changed() {
            let dir = temp_dir("symlink");
            std::fs::create_dir(dir.join("target")).unwrap();
            let target = dir.join("target/config.toml");
            std::fs::write(&target, SERVER).unwrap();
            let path = dir.join("config.toml");
            std::os::unix::fs::symlink(&target, &path).unwrap();
            let (_tx, rx) = broadcast::channel(1);
            let mut handle = ConfigWatcherHandle::new(&path, rx).await.unwrap();
            assert!(matches!(
                next_change(&mut handle).await,
                ConfigChange::General(_)
            ));
            // The watcher starts in its own task
            tokio::time::sleep(Duration::from_millis(500)).await;

            std::fs::write(&target, format!("{}{}", SERVER, SERVICE)).unwrap();
            expect_added(&mut handle).await;
            std::fs::remove_dir_all(&dir).unwrap();
        }

        // How Kubernetes updates a mounted ConfigMap: config.toml points into
        // ..data, which is a symlink to a versioned directory that gets swapped
        #[cfg(unix)]
        #[tokio::test]
        async fn kubernetes_configmap() {
            use std::os::unix::fs::symlink;
            let dir = temp_dir("configmap");
            std::fs::create_dir(dir.join("..v1")).unwrap();
            std::fs::write(dir.join("..v1/config.toml"), SERVER).unwrap();
            symlink("..v1", dir.join("..data")).unwrap();
            symlink("..data/config.toml", dir.join("config.toml")).unwrap();
            let (_tx, rx) = broadcast::channel(1);
            let mut handle = ConfigWatcherHandle::new(&dir.join("config.toml"), rx)
                .await
                .unwrap();
            assert!(matches!(
                next_change(&mut handle).await,
                ConfigChange::General(_)
            ));
            // The watcher starts in its own task
            tokio::time::sleep(Duration::from_millis(500)).await;

            std::fs::create_dir(dir.join("..v2")).unwrap();
            std::fs::write(
                dir.join("..v2/config.toml"),
                format!("{}{}", SERVER, SERVICE),
            )
            .unwrap();
            symlink("..v2", dir.join("..data_tmp")).unwrap();
            std::fs::rename(dir.join("..data_tmp"), dir.join("..data")).unwrap();
            std::fs::remove_dir_all(dir.join("..v1")).unwrap();
            expect_added(&mut handle).await;
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
