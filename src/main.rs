use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use libsql::Builder;
use papr::{get_db_path, handle_add, handle_remove};

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
    Remove { query: String },
    /// Compile and open the Typst summary
    Open { query: String },
    /// Sync the DB
    Sync,
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
        Commands::Remove { query } => handle_remove(&conn, query).await?,
        Commands::Open { query } => println!("Open stub for ID: {}", query),
        Commands::Sync => println!("Sync stub"),
    }

    Ok(())
}
