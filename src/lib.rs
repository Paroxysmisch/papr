mod search;

use anyhow::{Context, Result};
use chrono::Local;
use directories::ProjectDirs;
use inquire::{Confirm, Editor, MultiSelect, Select, Text};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fmt, fs};

use crate::search::PaperMatch;

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

async fn get_tag_selections(conn: &libsql::Connection) -> Result<Vec<String>> {
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

    Ok(final_tag_names)
}

async fn tag_paper(conn: &libsql::Connection, paper_id: u32, tag_names: Vec<String>) -> Result<()> {
    for tag_name in tag_names {
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

    Ok(())
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
    let citation = Editor::new("Paper citation:")
        .with_help_message("Save and exit editor to confirm changes.")
        .prompt_skippable()
        .context("Invalid citation.")?;
    let final_tag_names = get_tag_selections(conn).await?;

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

    // Create `main.typ` entry point
    let typ_content = format!("= Notes on: {}\n\nLink: {}\n", title, url);
    fs::write(summary_path.join("main.typ"), typ_content)?;

    // Update papers table
    conn.execute(
        "INSERT OR REPLACE INTO papers (canonical_base_path, url, date_added, citation) VALUES (?1, ?2, ?3, ?4)",
        (
            canonical_base_path.clone(),
            url.clone(),
            Local::now().format("%Y-%m-%d").to_string(),
            citation.unwrap_or_default(),
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

    tag_paper(conn, paper_id, final_tag_names).await?;

    println!("Successfully added '{}' to your library!", title);
    Ok(())
}

pub async fn handle_remove(conn: &libsql::Connection, query: String) -> Result<()> {
    let matching_papers = search::fuzzy_search_papers(conn, &query).await?;
    if matching_papers.is_empty() {
        anyhow::bail!("No papers found matching '{}'", query);
    }
    let paper_selections = MultiSelect::new(
        "Select papers to remove (Space to toggle, Enter to confirm):",
        matching_papers,
    )
    .prompt()
    .context("No papers selected for deletion.")?;

    for PaperMatch {
        id,
        canonical_base_path,
        ..
    } in paper_selections
    {
        // Delete the paper (this triggers cascade to clear paper_tags)
        conn.execute("DELETE FROM papers WHERE id = ?1", [id])
            .await?;

        // Prune orphan tags that no longer belong to any paper
        conn.execute(
            "DELETE FROM tags 
             WHERE id NOT IN (SELECT DISTINCT tag_id FROM paper_tags)",
            (),
        )
        .await?;

        fs::remove_dir_all(&canonical_base_path)?;
    }

    Ok(())
}

pub async fn handle_search(
    conn: &libsql::Connection,
    query: String,
    tags: Option<Vec<String>>,
    pdf: bool,
) -> Result<()> {
    if pdf {
        let results = search::fuzzy_search_pdfs(conn, &query, tags).await?;
        for pdf_match_result in results {
            println!(
                "Paper name: {} ({})\nPage: {}\nExcerpt: {}\n",
                Path::new(&pdf_match_result.canonical_path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Unknown"),
                pdf_match_result.canonical_path,
                pdf_match_result.page,
                pdf_match_result.excerpt
            );
        }
    } else {
        let results = search::fuzzy_search_typst(conn, &query, tags).await?;
        for typst_match_result in results {
            println!(
                "Paper name: {} ({})\nLine: {}\nExcerpt: {}\n",
                Path::new(&typst_match_result.canonical_path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Unknown"),
                typst_match_result.canonical_path,
                typst_match_result.line_number,
                typst_match_result.excerpt
            );
        }
    }

    Ok(())
}

pub async fn handle_retag(conn: &libsql::Connection, query: String) -> Result<()> {
    let matching_papers = search::fuzzy_search_papers(conn, &query).await?;
    if matching_papers.is_empty() {
        anyhow::bail!("No papers found matching '{}'", query);
    }
    let paper_selection = Select::new("Select paper to retag:", matching_papers)
        .prompt()
        .context("No paper selected for retagging.")?;

    let paper_id = paper_selection.id;

    let final_tag_names = get_tag_selections(conn).await?;

    // Clear existing associations for this paper
    conn.execute("DELETE FROM paper_tags WHERE paper_id = ?1", [paper_id])
        .await?;

    // Prune Orphaned Tags
    // This deletes tags that are no longer linked to ANY paper
    conn.execute(
        "DELETE FROM tags WHERE id NOT IN (SELECT DISTINCT tag_id FROM paper_tags)",
        (),
    )
    .await?;

    tag_paper(conn, paper_id, final_tag_names).await?;

    Ok(())
}

pub async fn handle_cite(conn: &libsql::Connection, query: String) -> Result<()> {
    let matching_papers = search::fuzzy_search_papers(conn, &query).await?;
    if matching_papers.is_empty() {
        anyhow::bail!("No papers found matching '{}'", query);
    }
    let paper_selection = Select::new("Select paper to retag:", matching_papers)
        .prompt()
        .context("No paper selected for retagging.")?;

    let paper_id = paper_selection.id;

    // Fetch the current citation value
    let mut rows = conn
        .query("SELECT citation FROM papers WHERE id = ?1", [paper_id])
        .await?;

    let current_citation: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        anyhow::bail!("Paper ID {} not found in database.", paper_id);
    };

    println!("\nCurrent Citation:\n{}\n", current_citation);

    let new_citation = Editor::new("Edit citation:")
        .with_predefined_text(&current_citation)
        .with_help_message("Save and exit editor to confirm changes.")
        .prompt()
        .context("Invalid citation input.")?;

    // Update the database if citation changed
    if new_citation != current_citation {
        conn.execute(
            "UPDATE papers SET citation = ?1 WHERE id = ?2",
            [new_citation, paper_id.to_string()],
        )
        .await?;
        println!("Citation updated successfully!");
    } else {
        println!("No changes made.");
    }

    Ok(())
}

pub async fn handle_notes(conn: &libsql::Connection, query: String) -> Result<()> {
    let matching_papers = search::fuzzy_search_papers(conn, &query).await?;
    if matching_papers.is_empty() {
        anyhow::bail!("No papers found matching '{}'", query);
    }
    let paper_selection = Select::new("Select paper to compile notes for:", matching_papers)
        .prompt()
        .context("No paper selected.")?;

    let base_path_str = paper_selection.canonical_base_path;
    let summary_dir = Path::new(&base_path_str).join("summary");

    // Locate the source .typ file
    let mut typst_file = summary_dir.join("main.typ");
    if !typst_file.exists() {
        let first_typ = std::fs::read_dir(&summary_dir)?
            .filter_map(|e| e.ok())
            .find(|e| e.path().extension().is_some_and(|ext| ext == "typ"))
            .map(|e| e.path());

        match first_typ {
            Some(path) => typst_file = path,
            None => anyhow::bail!("No .typ files found in {:?}", summary_dir),
        }
    }

    let output_pdf = typst_file.with_extension("pdf");

    // Force an initial compile so the file always exists
    println!("Performing initial build...");
    Command::new("typst")
        .arg("compile")
        .arg(&typst_file)
        .arg(&output_pdf)
        .status()?;

    // Open the PDF viewer first
    if output_pdf.exists() {
        println!("Opening PDF viewer...");
        let _ = open::that(&output_pdf);
    }

    // Run 'typst watch', blocking the current terminal session
    println!("Watch mode started for {}...", typst_file.display());
    println!("Press Ctrl+C to stop watching.");

    let mut child = Command::new("typst")
        .arg("watch")
        .arg(&typst_file)
        .arg(&output_pdf)
        .spawn() // Use spawn instead of status so we can manage the process if needed
        .context("Failed to start 'typst watch'. Is it installed?")?;

    let status = child.wait()?;

    if !status.success() {
        anyhow::bail!("Typst watch exited with an error.");
    }

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
