use anyhow::{Context, Result};
use chrono::Local;
use directories::ProjectDirs;
use inquire::{Confirm, MultiSelect, Text};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::{fmt, fs};

// What is stored in `metadata.toml`,
// and is thus user modifiable
#[derive(Serialize, Deserialize)]
struct PaperMetadata {
    url: String,
    date_added: String, // Format stored is `yyyy-mm-dd`
    tags: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum TagSelection {
    Tag {
        tag_name: String,
        usage_count: usize,
        tag_id: i32,
    },
    AddNewTag,
}

impl fmt::Display for TagSelection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tag {
                tag_name,
                usage_count,
                tag_id,
            } => {
                write!(
                    f,
                    "Tag name: {}, Usage count: {}, Tag ID: {}",
                    tag_name, usage_count, tag_id
                )
            }
            Self::AddNewTag => {
                write!(f, "Add new tag...",)
            }
        }
    }
}

pub async fn handle_add(conn: &libsql::Connection) -> Result<()> {
    // Prompt for title and URL
    let title = Text::new("Paper title (used for directory name):")
        .prompt()
        .context("Invalid title.")?;
    let directory_name = title.to_lowercase().replace(" ", "_");
    let url = Text::new("Paper PDF URL:")
        .prompt()
        .context("Invalid URL.")?;
    let mut cur_tags = get_all_tags(conn).await?;
    cur_tags.push(TagSelection::AddNewTag);
    cur_tags.sort();
    let tag_selections =
        MultiSelect::new("Select tags (Space to toggle, Enter to confirm):", cur_tags).prompt()?;
    let mut final_tag_names = Vec::new();

    for selection in tag_selections {
        match selection {
            TagSelection::AddNewTag => {
                // Prompt for custom additional tags
                let new_tags_input = Text::new(
                    "Enter new tags (separated by commas). Note that tags are case insensitive:",
                )
                .with_help_message("e.g. gnn, transformer, mamba")
                .prompt()?;

                for t in new_tags_input.split(',') {
                    let trimmed = t.trim().to_string();
                    if !trimmed.is_empty() {
                        final_tag_names.push(trimmed);
                    }
                }
            }
            TagSelection::Tag {
                tag_name,
                usage_count: _,
                tag_id: _,
            } => final_tag_names.push(tag_name),
        }
    }

    final_tag_names.sort();
    final_tag_names.dedup();

    // Start downloading PDF before creating any directories for easy clean-up,
    // in case of failure to retrieve from URL
    println!("Downloading PDF...");
    let response = reqwest::get(url.as_str())
        .await
        .context("Error downloading PDF.")?;
    let content = response
        .bytes()
        .await
        .context("Did not receive response when downloading PDF.")?;

    // Setup directory structure for this new paper
    // Prompt user to overwrite if the canonicalized path already exists
    // Note that the entire path, not just the paper name has to match
    let base_path = Path::new(".").join(&directory_name);
    let summary_path = base_path.join("summary");
    let canonical_base_path = fs::canonicalize(Path::new("."))
        .context("Error canonicalizing current directory.")?
        .join(&directory_name)
        .into_os_string()
        .into_string()
        .unwrap();

    let mut rows = conn
        .query(
            "SELECT canonical_base_path FROM papers WHERE canonical_base_path = ?1",
            [canonical_base_path.clone()],
        )
        .await?;

    if let Some(row) = rows.next().await? {
        let existing_canonicalized_path: String = row.get(0)?;

        let ans = Confirm::new(&format!(
            "Paper '{}' already exists in the database at {}. Overwrite?",
            title, existing_canonicalized_path
        ))
        .with_default(false)
        .with_help_message("This will update the DB entry and remove the old paper directory. This means that your notes will be deleted.")
        .prompt()?;

        if ans {
            fs::remove_dir_all(&base_path)?;
        } else {
            println!("Add operation cancelled.");
            return Ok(());
        }
    }

    fs::create_dir_all(&base_path).context("Error creating base directory.")?;
    fs::create_dir_all(&summary_path).context("Error creating summary directory")?;

    // Download PDF
    let pdf_file_path = base_path.join("paper.pdf");
    let mut file = fs::File::create(&pdf_file_path)?;
    std::io::copy(&mut content.as_ref(), &mut file)?;

    // Create Metadata TOML
    let metadata = PaperMetadata {
        url: url.clone(),
        date_added: Local::now().format("%Y-%m-%d").to_string(),
        tags: vec![], // Empty initially for the user to fill in later
    };
    let toml_string = toml::to_string_pretty(&metadata).context("Error parsing metadata TOML.")?;
    let metadata_path = base_path.join("metadata.toml");
    fs::write(&metadata_path, toml_string)?;

    // Create `main.typ` entry point
    let typ_content = format!("= Notes on: {}\n\nLink: {}\n", title, url);
    fs::write(summary_path.join("main.typ"), typ_content)?;

    // Update papers table
    conn.execute(
        "INSERT OR REPLACE INTO papers (canonical_base_path, url, date_added) VALUES (?1, ?2, ?3)",
        (
            canonical_base_path.clone(),
            metadata.url.clone(),
            metadata.date_added.clone(),
        ),
    )
    .await
    .context("Error updating papers table.")?;

    // Update the tags and paper_tags tables
    let paper_id: u32 = conn
        .query(
            "select id from papers where canonical_base_path = ?1",
            [canonical_base_path.clone()],
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;

    for tag_name in final_tag_names {
        conn.execute(
            "INSERT OR IGNORE INTO tags (name) VALUES (?1)",
            [tag_name.clone()],
        )
        .await
        .context("Error updating tags table")?;

        let tag_id: u32 = conn
            .query("select id from tags where name = ?1", [tag_name])
            .await?
            .next()
            .await?
            .unwrap()
            .get(0)?;

        // Link paper to tag
        conn.execute(
            "INSERT OR IGNORE INTO paper_tags (paper_id, tag_id) VALUES (?1, ?2)",
            (paper_id, tag_id),
        )
        .await?;
    }

    println!("Successfully added '{}' to your library!", title);
    Ok(())
}

pub fn get_db_path() -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "", "papr")
        .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    let data_dir = proj_dirs.data_dir();
    fs::create_dir_all(data_dir)?;
    Ok(data_dir.join("papr.db"))
}

async fn get_all_tags(conn: &libsql::Connection) -> Result<Vec<TagSelection>> {
    let mut rows = conn
        .query(
            "SELECT t.name, COUNT(pt.paper_id), t.id as count
                FROM tags t
                LEFT JOIN paper_tags pt ON t.id = pt.tag_id
                GROUP BY t.name
                ORDER BY count DESC;",
            (),
        )
        .await?;
    let mut tags = Vec::new();

    while let Some(row) = rows.next().await? {
        let tag_name: String = row.get(0)?;
        let usage_count: u32 = row.get(1)?;
        let tag_id: i32 = row.get(2)?;
        tags.push(TagSelection::Tag {
            tag_name,
            usage_count: usage_count as usize,
            tag_id,
        });
    }

    Ok(tags)
}
