use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;

// Target file structure
#[derive(Serialize, Deserialize, Debug, Clone)]
struct FilamentInfo {
    filament_id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

// Source files structure
#[derive(Serialize, Deserialize, Debug, Clone)]
struct FilamentPointer {
    pub name: String,
    pub sub_path: String,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
struct BBL {
    filament_list: Vec<FilamentPointer>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FilamentFile {
    name: String,
    filament_id: String,
    description: Option<String>,
}

pub async fn get_custom_filaments_index() -> Result<String, Box<dyn std::error::Error>> {
    let mut unique_filaments = HashMap::new();
    let bambu_input_path_under_home;
    let orca_input_path_under_home;
    if cfg!(target_os = "windows") {
        println!("Running on Windows!");
        bambu_input_path_under_home = "AppData\\Roaming\\BambuStudio\\user";
        orca_input_path_under_home = "AppData\\Roaming\\OrcaSlicer\\user";
    } else if cfg!(target_os = "macos") {
        println!("Running on macOS!");
        bambu_input_path_under_home = "Library/Application Support/BambuStudio/user";
        orca_input_path_under_home = "Library/Application Support/OrcaSlicer/user";
    } else if cfg!(target_os = "linux") {
        println!("Running on Linux!");
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Linux is currently not supported",
        )));
    } else {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Unknown Operating System",
        )));
    }

    let bambu_input_path;
    let orca_input_path;
    let mut bambu_or_orca_found = false;
    if let Some(home_dir) = dirs::home_dir() {
        bambu_input_path = home_dir.join(bambu_input_path_under_home);
        if fs::metadata(&bambu_input_path).await.is_ok() {
            bambu_or_orca_found = true;
            visit_dirs_custom(&bambu_input_path, &mut unique_filaments).await?;
        }
        orca_input_path = home_dir.join(orca_input_path_under_home);
        if fs::metadata(&orca_input_path).await.is_ok() {
            bambu_or_orca_found = true;
            visit_dirs_custom(&orca_input_path, &mut unique_filaments).await?;
        }
    } else {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "User home directory not identified",
        )));
    }
    if !bambu_or_orca_found {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "Neither OrcaSlicer nor BambuStudio folders found:\n{}\n{}",
                bambu_input_path.display(),
                orca_input_path.display()
            ),
        )));
    }
    file_info_string(unique_filaments).await
}

fn fix_info_file(input: String) -> String {
    // Process each line separately to handle the empty value case correctly
    input
        .lines()
        .map(|line| {
            if let Some(equals_pos) = line.find('=') {
                let (key_part, value_part) = line.split_at(equals_pos + 1);
                let value_trimmed = value_part.trim();

                // Keep the key part (including equals sign) as is
                format!("{} \"{}\"", key_part, value_trimmed)
            } else {
                // If there's no equals sign, return the line unchanged
                line.to_string()
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Info {
    base_id: String,
    setting_id: String,
}

async fn visit_dirs_custom(
    dir: &PathBuf,
    unique_filaments: &mut HashMap<String, FilamentInfo>,
) -> Result<(), Box<dyn std::error::Error>> {
    // for entry in fs::read_dir(dir).await? {
    let mut entries = fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        if path.is_dir() {
            Box::pin(visit_dirs_custom(&path, unique_filaments)).await?; // Recurse into subdirectory
        } else if path.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext.to_str() == Some("info"))
        {
            let info_file_str = fs::read_to_string(&path).await?;
            let info_file_str = fix_info_file(info_file_str);

            if let Ok(info) = toml::from_str::<Info>(&info_file_str) {
                if info.base_id.is_empty() {
                    let mut json_file = PathBuf::from(path);
                    json_file.set_extension("json");
                    let filament_file_str = fs::read_to_string(&json_file).await?;

                    if let Ok(filament_file) =
                        serde_json::from_str::<FilamentFile>(&filament_file_str)
                    {
                        unique_filaments
                            .entry(filament_file.filament_id.to_string())
                            .or_insert(FilamentInfo {
                                filament_id: filament_file.filament_id,
                                name: filament_file
                                    .name
                                    .split("@")
                                    .next()
                                    .unwrap()
                                    .trim()
                                    .to_string(),
                                description: filament_file.description,
                            });
                    } else {
                        // println!("No required data in : {}", filament_file_path.display());
                    }
                }
            }
        } else {
            // println!("NO - {} - {}", path.is_file(), path.display());
        }
    }
    Ok(())
}

async fn file_info_string(
    unique_filaments: HashMap<String, FilamentInfo>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut result: Vec<_> = unique_filaments.values().cloned().collect();
    result.sort_by(|a, b| {
        a.filament_id
            .cmp(&b.filament_id)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| {
                a.description
                    .as_ref()
                    .unwrap_or(&"".to_string())
                    .cmp(&b.description.as_ref().unwrap_or(&"".to_string()))
            })
    });
    let json_output = serde_json::to_string_pretty(&result)?;
    Ok(json_output)
}
