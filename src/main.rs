use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use libsql::Builder;
use papr::{
    get_db_path, handle_add, handle_cite, handle_notes, handle_remove, handle_retag, handle_search,
};

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
    Search {
        query: String,

        /// Filter by tags (comma-separated: --tags=math,physics)
        #[arg(short, long, value_delimiter = ',', num_args = 1..)]
        tags: Option<Vec<String>>,

        /// Also search inside the PDF text
        #[arg(long)]
        pdf: bool,
    },
    /// Remove a paper and its data
    Remove { query: String },
    /// Compile and open the Typst summary
    Notes { query: String },
    /// Sync the DB
    Sync,
    /// Change the tags assigned to a paper
    Tag { query: String },
    /// Change the citation assigned to a paper
    Cite { query: String },
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
            date_added TEXT NOT NULL,
            citation TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS tags (
            id INTEGER PRIMARY KEY,
            name TEXT UNIQUE
        );
        CREATE TABLE IF NOT EXISTS paper_tags (
            paper_id INTEGER,
            tag_id INTEGER,
            FOREIGN KEY(paper_id) REFERENCES papers(id) ON DELETE CASCADE,
            FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE
        );",
    )
    .await
    .context("Error initializing DB schema.")?;

    println!();

    match cli.command {
        Commands::Add => handle_add(&conn).await?,
        Commands::Search { query, tags, pdf } => handle_search(&conn, query, tags, pdf).await?,
        Commands::Remove { query } => handle_remove(&conn, query).await?,
        Commands::Notes { query } => handle_notes(&conn, query).await?,
        Commands::Sync => println!("Sync stub"),
        Commands::Tag { query } => handle_retag(&conn, query).await?,
        Commands::Cite { query } => handle_cite(&conn, query).await?,
    }

    Ok(())
}
