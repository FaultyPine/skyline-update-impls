use std::{io, fs};
use semver::Version;
use std::path::{Path, PathBuf};
use update_protocol::InstallLocation;
use serde::{Serialize, Deserialize};
use color_eyre::eyre;

#[derive(Serialize, Deserialize, Clone)]
pub struct PluginFile {
    pub install_location: InstallLocation,
    pub filename: PathBuf,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PluginFolder {
    pub install_root_location: InstallLocation,
    pub root_name: PathBuf,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TomlMetadata {
    pub name: Option<String>,
    pub images: Option<Vec<PathBuf>>,
    pub description: Option<String>,
    pub changelog: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PluginToml {
    #[serde(with = "version_parse")]
    pub version: Version,

    pub name: String,

    pub beta: Option<bool>,

    pub files: Vec<PluginFile>,

    pub folders: Option<Vec<PluginFolder>>,

    #[serde(default, with = "version_parse_opt", skip_serializing_if = "Option::is_none")]
    pub skyline_version: Option<Version>,

    pub metadata: Option<TomlMetadata>,
}

mod version_parse {
    use core::fmt;
    use semver::Version;
    use serde::{Serializer, Deserializer, de::{self, Visitor}};

    pub fn serialize<S>(ver: &Version, ser: S) -> Result<S::Ok, S::Error>
        where S: Serializer
    {
        ser.collect_str(ver)
    }

    struct VerVisitor;

    impl<'de> Visitor<'de> for VerVisitor {
        type Value = Version;

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
                E: de::Error, {
            v.parse().map_err(|_| E::custom("Failed to parse version"))
        }

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a valid semver version string")
        }
    }

    pub fn deserialize<'de, D>(de: D) -> Result<Version, D::Error>
        where D: Deserializer<'de>
    {
        de.deserialize_string(VerVisitor)
    }
}

mod version_parse_opt {
    use semver::Version;
    use serde::{Serializer, Deserializer};

    pub fn serialize<S>(ver: &Option<Version>, ser: S) -> Result<S::Ok, S::Error>
        where S: Serializer
    {
        ser.collect_str(ver.as_ref().unwrap())
    }

    pub fn deserialize<'de, D>(de: D) -> Result<Option<Version>, D::Error>
        where D: Deserializer<'de>
    {
        Ok(super::version_parse::deserialize(de).ok())
    }
}

#[derive(Default)]
pub struct Metadata {
    pub name: Option<String>,
    pub images: Option<Vec<Vec<u8>>>,
    pub description: Option<String>,
    pub changelog: Option<String>,
}

pub struct Plugin {
    pub name: String,
    pub plugin_version: Version,
    pub files: Vec<(InstallLocation, Vec<u8>)>,
    pub skyline_version: Version,
    pub beta: bool,
    pub metadata: Metadata,
}

fn to_file(PluginFile { install_location, filename }: PluginFile, dir: &Path) -> eyre::Result<(InstallLocation, Vec<u8>)> {
    let path = if filename.is_absolute() {
        filename
    } else {
        dir.join(filename)
    };

    Ok((install_location, fs::read(path)?))
}

pub fn folder_to_plugin(dir: io::Result<fs::DirEntry>) -> eyre::Result<Option<Plugin>> {
    let path = dir?.path();
    if !path.is_dir() {
        return Ok(None)
    }
    let toml_path = path.join("plugin.toml");

    let plugin: PluginToml = toml::from_str(&fs::read_to_string(toml_path)?)?;

    let PluginToml { version, name, files, folders, skyline_version, beta, metadata } =  plugin;

    let mut files: Vec<(InstallLocation, Vec<u8>)> = files.into_iter().map(|file| to_file(file, &path)).collect::<eyre::Result<_>>()?;

    /* cwd joined with our current "plugin" I.E. mnt/..../HDR  */
    let plugin_path = &std::env::current_dir().unwrap().join(&path);        
    /* Name of current plugin */
    //let plugin_name = plugin_path.file_name().unwrap().to_str().unwrap();

    /* Handle directories */
    for folder in folders.unwrap_or_default() {

        /* cwd joined with current plugin joined with our current romfs folder  I.E. /mnt/..../HDR/HDR-Base   */
        let folder_dep_path = &plugin_path.join(Path::new(folder.root_name.to_str().unwrap()));
        /* Name of current folder dep */
        let folder_dep_name = folder_dep_path.file_name().unwrap().to_str().unwrap();

        let tar_name = folder_dep_path.file_stem().unwrap().to_str().unwrap().to_owned() + ".tar";
        let tar_path = plugin_path.join(tar_name.clone());

        let mut tar = tar::Builder::new(fs::File::create(tar_path.clone())?);

        /* recurse through folder and write files to the ZipWriter. */
        for file_from_folder in walkdir::WalkDir::new(folder_dep_path).contents_first(true) {
            let file_from_folder = file_from_folder?;
            if file_from_folder.path().is_dir() {
                continue;
            }

            let curr_absolute_dir = file_from_folder.path().to_str().unwrap().to_string();
            let curr_recurse_dir = &curr_absolute_dir.clone()[curr_absolute_dir.find("plugins").unwrap() ..];
            //println!("Zipping: {}", curr_recurse_dir.clone());

            let _ = tar.append_path_with_name(curr_recurse_dir, &curr_recurse_dir[curr_recurse_dir.find(folder_dep_name).unwrap()..]).unwrap();
        }
        let _ = tar.finish()?;

        let install_loc: &Path = match folder.install_root_location {
            InstallLocation::AbsolutePath(ref p) => Path::new(p),
            _ => {
                println!("Install location unknown... {:#?}", folder.install_root_location);
                Path::new("ERR")
            }
        };
        let mut install_loc = install_loc.to_str().unwrap().to_string();
        install_loc.push_str(".tar");

        let file_data = ( InstallLocation::AbsolutePath(install_loc), fs::read(&tar_path)? );

        files.push(file_data);

    }
    
    let metadata = metadata.map(|metadata| {
        Metadata {
            name: metadata.name,
            images: metadata.images.map(|x| x.iter().map(|path| fs::read(path).unwrap_or_default()).collect()),
            description: metadata.description,
            changelog: metadata.changelog.map(|path| fs::read_to_string(path).ok()).flatten()
        }
    }).unwrap_or_default();

    Ok(Some(Plugin {
        name,
        plugin_version: version,
        files,
        skyline_version: skyline_version.unwrap_or("0.0.0".parse().unwrap()),
        beta: beta.unwrap_or(false),
        metadata,
    }))
}

pub fn get() -> eyre::Result<Vec<Plugin>> {
    Ok(
        fs::read_dir("plugins")?
            .filter_map(|entry| {
                match folder_to_plugin(entry) {
                    Ok(x) => x,
                    Err(e) => {
                        println!("{}", e);
                        None
                    }
                }
            })
            .collect()
    )
}

/*pub fn print_default() {
    println!("{}", toml::to_string_pretty(&PluginToml {
        name: "name".to_owned(),
        version: "1.0.0".parse().unwrap(),
        files: vec![],
        skyline_version: None,
        beta: Some(false),
        metadata: None,
    }).unwrap());
}*/
