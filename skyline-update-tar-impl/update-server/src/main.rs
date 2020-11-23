mod hosted_plugins;

use notify::{Watcher, RecursiveMode, watcher};
use std::sync::mpsc::channel;
use std::time::Duration;

use std::fs;
use std::sync::Arc;
use std::path::Path;
use std::net::TcpListener;
use std::io::{prelude::*, BufReader};

use color_eyre::eyre;

use semver::Version;
use update_protocol::{InstallLocation, Request, UpdateResponse, ResponseCode, UpdateFile, PluginMetadata};

struct PluginFile {
    install: InstallLocation,
    data: Arc<Vec<u8>>,
    index: u64,
}

impl From<&PluginFile> for UpdateFile {
    fn from(file: &PluginFile) -> Self {
        UpdateFile {
            size: file.data.len(),
            download_index: file.index.clone(),
            install_location: file.install.clone()
        }
    }
}

struct Plugin {
    pub name: String,
    pub plugin_version: Version,
    pub files: Vec<PluginFile>,
    pub metadata_files: Vec<Arc<Vec<u8>>>,
    pub metadata: PluginMetadata,
    pub skyline_version: Version,
    pub beta: bool,
}

const PORT_NUM: u16 = 45000;

fn setup_plugin_ports() -> eyre::Result<(Vec<Plugin>, Vec<Arc<Vec<u8>>>)> {
    let plugins = hosted_plugins::get()?;

    let mut i = 0;
    let plugins: Vec<Plugin> = plugins.into_iter()
        .map(|plugin|{
            let hosted_plugins::Plugin {
                name, plugin_version, files, skyline_version, beta, metadata
            } = plugin;

            let files = files.into_iter()
                .map(|(install, data)|{
                    let index = i;
                    i += 1;
                    Ok(PluginFile {
                        install,
                        index,
                        data: Arc::new(data),
                    })
                })
                .collect::<eyre::Result<_>>()?;

            let hosted_plugins::Metadata {
                name: meta_name, images, changelog, description
            } = metadata;

            let image_count = images.as_ref().map(|x| x.len() as _).unwrap_or(0);

            let metadata = PluginMetadata {
                name: meta_name,
                description,
                images_index: i,
                image_count,
                changelog_index: i + image_count,
            };

            let metadata_files = images.into_iter()
                .map(|images| images.into_iter())
                .flatten()
                .map(|image| Arc::new(image))
                .chain(changelog.into_iter().map(|x| Arc::new(x.into_bytes())))
                .collect();

            Ok(Plugin {
                name,
                plugin_version,
                skyline_version,
                files,
                metadata_files,
                metadata,
                beta
            })
        })
        .collect::<eyre::Result<_>>()?;

    let files = plugins.iter()
        .map(|plugin| plugin.files.iter().map(|file| Arc::clone(&file.data)))
        .flatten()
        .collect();

    Ok((plugins, files))
}
#[allow(unused_assignments)]
fn main() -> eyre::Result<()> {
    color_eyre::install()?;

    //hosted_plugins::print_default();

    let plugins_dir = Path::new("plugins");
    if !plugins_dir.exists() {
        fs::create_dir(plugins_dir)?;
    }

    let (tx, rx) = channel();

    let mut watcher = watcher(tx, Duration::from_secs(10)).unwrap();

    watcher.watch("plugins", RecursiveMode::Recursive).unwrap();

    let (mut plugins, mut files) = setup_plugin_ports()?;
    let main_port = TcpListener::bind(("0.0.0.0", PORT_NUM))?;
    let download_port = TcpListener::bind(("0.0.0.0", PORT_NUM + 1))?;
    main_port.set_nonblocking(true)?;
    download_port.set_nonblocking(true)?;

    crossbeam::scope(move |scope|{
        loop {
            match rx.try_recv() {
                Ok(notify::DebouncedEvent::Error(err, Some(path))) => {
                    println!("File watch error at path {}: {}", path.display(), err);
                }
                Ok(notify::DebouncedEvent::Error(err, None)) => {
                    println!("File watch error: {}", err);
                }
                Ok(event) => {
                    /* dont refresh plugins on zip creation/write. This prevents infinite plugin refreshing with zip creation */
                    match event {
                        notify::DebouncedEvent::Create(path) => {
                            if path.extension().unwrap_or_default() == "tar" {
                                continue;
                            }
                        },
                        notify::DebouncedEvent::Write(path) => {
                            if path.extension().unwrap_or_default() == "tar" {
                                continue;
                            }
                        },
                        notify::DebouncedEvent::NoticeWrite(path) => {
                            if path.extension().unwrap_or_default() == "tar" {
                                continue;
                            }
                        }
                        _ => ()
                    };
                    println!("Change detected: refreshing plugins...");
                    // clear plugins (close sockets)
                    plugins = Vec::with_capacity(0);
                    // setup new plugins
                    let (x, y) = setup_plugin_ports()?;
                    plugins = x;
                    files = y;
                },
                Err(_) => {}
            }

            while let Ok((socket, _)) = main_port.accept() {
                let mut socket = BufReader::new(socket);
                let plugins = &plugins;
                let mut packet = String::new();
                let _ = socket.read_line(&mut packet);
                macro_rules! respond {
                    ($expr:expr) => {{
                        let response = $expr;
                        let mut socket = socket.into_inner();
                        let _ = socket.write(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes());
                        let _ = socket.shutdown(std::net::Shutdown::Both);
                    }}
                }
                match serde_json::from_str::<Request>(&packet) {
                    Ok(Request::Update { plugin_name, plugin_version, beta, .. }) => {
                        let beta = beta.unwrap_or(false);
                        let plugin = plugins.iter().filter(|plugin| {
                            plugin.name == plugin_name && (beta || !plugin.beta)
                        }).max_by_key(|plugin| &plugin.plugin_version);

                        let response = if let Some(plugin) = plugin {
                            if let Ok(current_version) = plugin_version.parse::<Version>() {
                                if current_version < plugin.plugin_version {
                                    UpdateResponse {
                                        code: ResponseCode::Update,
                                        update_plugin: true,
                                        update_skyline: false,
                                        plugin_name,
                                        new_plugin_version: plugin.plugin_version.to_string(),
                                        new_skyline_version: None,
                                        required_files: plugin.files.iter().map(|file| file.into()).collect()
                                    }
                                } else {
                                    UpdateResponse::no_update()
                                }
                            } else {
                                UpdateResponse::invalid_request()
                            }
                        } else {
                            UpdateResponse::plugin_not_found()
                        };

                        respond!(response);
                    }
                    Ok(Request::Metadata { plugin_name, beta, .. }) => {
                        let beta = beta.unwrap_or(false);
                        let plugin = plugins.iter().filter(|plugin| {
                            plugin.name == plugin_name && (beta || !plugin.beta)
                        }).max_by_key(|plugin| &plugin.plugin_version);

                        if let Some(plugin) = plugin {
                            respond!(&plugin.metadata)
                        }
                    }
                    _ => respond!(UpdateResponse::invalid_request()),
                }
            }

            while let Ok((mut socket, _)) = download_port.accept() {
                let mut buf = [0; 8];
                if let Ok(_) = socket.read_exact(&mut buf) {
                    let index = u64::from_be_bytes(buf) as usize;
                    if let Some(file) = files.get(index) {
                        let data = Arc::clone(&file);
                        scope.spawn(move |_| {
                            let _ = socket.write_all(&data);
                        });
                    }
                } else {
                    println!("Failed to read index");
                    let _ = socket.shutdown(std::net::Shutdown::Both);
                }
            }

            std::thread::sleep(Duration::from_millis(10));
        }
    }).unwrap()
}
