use anyhow::{Context, Result};
use chrono::Local;
use clap::{Parser, Subcommand};
use directories::ProjectDirs;
use inquire::{Confirm, Text};
use libsql::Builder;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "papr", about = "PhD paper management system.", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Add a new paper from a URL
    Add,
    /// Search through indexed papers
    Search { query: String },
    /// Remove a paper and its data
    Remove { id: i32 },
    /// Compile and open the Typst summary
    Open { id: i32 },
}

// What is stored in `metadata.toml`,
// and is thus user modifiable
#[derive(Serialize, Deserialize)]
struct PaperMetadata {
    url: String,
    date_added: String, // Format stored is `yyyy-mm-dd`
    tags: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let db_path = get_db_path()?;

    // Initialize DB
    println!("DB file at {:?}", db_path);
    let db = Builder::new_local(db_path).build().await?;
    let conn = db.connect()?;

    // Initialize Schema
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS papers (
            id INTEGER PRIMARY KEY,
            canonical_base_path TEXT NOT NULL UNIQUE,
            url TEXT NOT NULL,
            date_added TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS tags (
            id INTEGER PRIMARY KEY,
            name TEXT UNIQUE
        );
        CREATE TABLE IF NOT EXISTS paper_tags (
            paper_id INTEGER,
            tag_id INTEGER,
            FOREIGN KEY(paper_id) REFERENCES papers(id),
            FOREIGN KEY(tag_id) REFERENCES tags(id)
        );",
    )
    .await
    .context("Error initializing DB schema.")?;

    match cli.command {
        Commands::Add => handle_add(&conn).await?,
        Commands::Search { query } => println!("Search stub for: {}", query),
        Commands::Remove { id } => println!("Remove stub for ID: {}", id),
        Commands::Open { id } => println!("Open stub for ID: {}", id),
    }

    Ok(())
}

async fn handle_add(conn: &libsql::Connection) -> Result<()> {
    // Prompt for title and URL
    let title = Text::new("Paper title (used for directory name):")
        .prompt()
        .context("Invalid title.")?;
    let directory_name = title.to_lowercase().replace(" ", "_");
    let url = Text::new("Paper PDF URL:")
        .prompt()
        .context("Invalid URL.")?;

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

    // Update database
    conn.execute(
        "INSERT OR REPLACE INTO papers (canonical_base_path, url, date_added) VALUES (?1, ?2, ?3)",
        (
            canonical_base_path.clone(),
            metadata.url.clone(),
            metadata.date_added.clone(),
        ),
    )
    .await
    .context("Error updating database.")?;

    println!("Successfully added '{}' to your library!", title);
    Ok(())
}

fn get_db_path() -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "", "papr")
        .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    let data_dir = proj_dirs.data_dir();
    fs::create_dir_all(data_dir)?;
    Ok(data_dir.join("papr.db"))
}
