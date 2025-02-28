//! command-line interface for running various tasks and commands
//! related to the application. It allows developers to interact with the
//! application via the command line.
//!
//! # Example
//!
//! ```rust,ignore
//! use myapp::app::App;
//! use loco_rs::cli;
//! use migration::Migrator;
//!
//! #[tokio::main]
//! async fn main() {
//!     cli::main::<App, Migrator>().await
//! }
//! ```

use std::collections::BTreeMap;

cfg_if::cfg_if! {
    if #[cfg(feature = "with-db")] {
        use sea_orm_migration::MigratorTrait;
        use crate::doctor;
        use crate::boot::run_db;
        use std::process::exit;
    } else {}
}

use std::str::FromStr;

use clap::{Parser, Subcommand};

use crate::{
    app::{AppContext, Hooks},
    boot::{create_app, create_context, list_endpoints, run_task, start, RunDbCommand, StartMode},
    environment::{resolve_from_env, Environment},
    gen::{self, Component},
    logger, Result,
};

const DEFAULT_ENVIRONMENT: &str = "development";
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Playground {
    /// Specify the environment
    #[arg(short, long, global = true, help = &format!("Specify the environment [default: {}]", DEFAULT_ENVIRONMENT))]
    environment: Option<String>,
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Specify the environment
    #[arg(short, long, global = true, help = &format!("Specify the environment [default: {}]", DEFAULT_ENVIRONMENT))]
    environment: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start an app
    Start {
        /// start worker
        #[arg(short, long, action)]
        worker: bool,
        /// start same-process server and worker
        #[arg(short, long, action)]
        server_and_worker: bool,
    },
    #[cfg(feature = "with-db")]
    /// Perform DB operations
    Db {
        #[command(subcommand)]
        command: DbCommands,
    },
    /// Describe all application endpoints
    Routes {},
    /// Run a custom task
    Task {
        /// Task name (identifier)
        name: Option<String>,
        /// Task params (e.g. <my_task> foo:bar baz:qux)
        #[clap(value_parser = parse_key_val::<String,String>)]
        params: Vec<(String, String)>,
    },
    /// code generation creates a set of files and code templates based on a
    /// predefined set of rules.
    Generate {
        /// What to generate
        #[command(subcommand)]
        component: ComponentArg,
    },
    #[cfg(feature = "with-db")]
    /// Validate and diagnose configurations.
    Doctor {},
    /// Display the app version
    Version {},
}

#[derive(Subcommand)]
enum ComponentArg {
    #[cfg(feature = "with-db")]
    /// Generates a new model file for defining the data structure of your
    /// application, and test file logic.
    Model {
        /// Name of the thing to generate
        name: String,

        /// Is it a link table? Use this in many-to-many relations
        #[arg(short, long, action)]
        link: bool,

        /// Model fields, eg. title:string hits:int
        #[clap(value_parser = parse_key_val::<String,String>)]
        fields: Vec<(String, String)>,
    },
    #[cfg(feature = "with-db")]
    /// Generates a new migration file
    Migration {
        /// Name of the migration to generate
        name: String,
    },
    #[cfg(feature = "with-db")]
    /// Generates a CRUD scaffold, model and controller
    Scaffold {
        /// Name of the thing to generate
        name: String,

        /// Model fields, eg. title:string hits:int
        #[clap(value_parser = parse_key_val::<String,String>)]
        fields: Vec<(String, String)>,
    },
    /// Generate a new controller with the given controller name, and test file.
    Controller {
        /// Name of the thing to generate
        name: String,
    },
    /// Generate a Task based on the given name
    Task {
        /// Name of the thing to generate
        name: String,
    },
    /// Generate worker
    Worker {
        /// Name of the thing to generate
        name: String,
    },
    /// Generate mailer
    Mailer {
        /// Name of the thing to generate
        name: String,
    },
    /// Generate a deployment infrastructure
    Deployment {},
}

impl From<ComponentArg> for Component {
    fn from(value: ComponentArg) -> Self {
        match value {
            #[cfg(feature = "with-db")]
            ComponentArg::Model { name, link, fields } => Self::Model { name, link, fields },
            #[cfg(feature = "with-db")]
            ComponentArg::Migration { name } => Self::Migration { name },
            #[cfg(feature = "with-db")]
            ComponentArg::Scaffold { name, fields } => Self::Scaffold { name, fields },
            ComponentArg::Controller { name } => Self::Controller { name },
            ComponentArg::Task { name } => Self::Task { name },
            ComponentArg::Worker { name } => Self::Worker { name },
            ComponentArg::Mailer { name } => Self::Mailer { name },
            ComponentArg::Deployment {} => Self::Deployment {},
        }
    }
}

#[derive(Subcommand)]
enum DbCommands {
    /// Migrate schema (up)
    Migrate,
    /// Drop all tables, then reapply all migrations
    Reset,
    /// Migration status
    Status,
    /// Generate entity .rs files from database schema
    Entities,
    /// Truncate data in tables (without dropping)
    Truncate,
}

impl From<DbCommands> for RunDbCommand {
    fn from(value: DbCommands) -> Self {
        match value {
            DbCommands::Migrate => Self::Migrate,
            DbCommands::Reset => Self::Reset,
            DbCommands::Status => Self::Status,
            DbCommands::Entities => Self::Entities,
            DbCommands::Truncate => Self::Truncate,
        }
    }
}

/// Parse a single key-value pair
fn parse_key_val<T, U>(
    s: &str,
) -> std::result::Result<(T, U), Box<dyn std::error::Error + Send + Sync>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
    U: std::str::FromStr,
    U::Err: std::error::Error + Send + Sync + 'static,
{
    let pos = s
        .find(':')
        .ok_or_else(|| format!("invalid KEY=value: no `:` found in `{s}`"))?;
    Ok((s[..pos].parse()?, s[pos + 1..].parse()?))
}

#[cfg(feature = "with-db")]
/// run playgroup code
///
/// # Errors
///
/// When could not create app context
pub async fn playground<H: Hooks>() -> Result<AppContext> {
    let cli = Playground::parse();
    let environment = cli
        .environment
        .or_else(resolve_from_env)
        .unwrap_or_else(|| DEFAULT_ENVIRONMENT.to_string());
    let app_context = create_context::<H>(&environment).await?;
    Ok(app_context)
}

/// # Main CLI Function
///
/// The `main` function is the entry point for the command-line interface (CLI)
/// of the application. It parses command-line arguments, interprets the
/// specified commands, and performs corresponding actions. This function is
/// generic over `H` and `M`, where `H` represents the application hooks and `M`
/// represents the migrator trait for handling database migrations.
///
/// # Errors
///
/// Returns an any error indicating success or failure during the CLI execution.
///
/// # Example
///
/// ```rust,ignore
/// use myapp::app::App;
/// use loco_rs::cli;
/// use migration::Migrator;
///
/// #[tokio::main]
/// async fn main()  {
///     cli::main::<App, Migrator>().await
/// }
/// ```
#[cfg(feature = "with-db")]
pub async fn main<H: Hooks, M: MigratorTrait>() -> eyre::Result<()> {
    let cli = Cli::parse();
    let environment = cli
        .environment
        .or_else(resolve_from_env)
        .unwrap_or_else(|| DEFAULT_ENVIRONMENT.to_string());
    match cli.command {
        Commands::Start {
            worker,
            server_and_worker,
        } => {
            let start_mode = if worker {
                StartMode::WorkerOnly
            } else if server_and_worker {
                StartMode::ServerAndWorker
            } else {
                StartMode::ServerOnly
            };

            let boot_result = create_app::<H, M>(start_mode, &environment).await?;
            start(boot_result).await?;
        }
        #[cfg(feature = "with-db")]
        Commands::Db { command } => {
            let app_context = create_context::<H>(&environment).await?;
            run_db::<H, M>(&app_context, command.into()).await?;
        }
        Commands::Routes {} => show_list_endpoints::<H>(),
        Commands::Task { name, params } => {
            let mut hash = BTreeMap::new();
            for (k, v) in params {
                hash.insert(k, v);
            }
            let app_context = create_context::<H>(&environment).await?;
            run_task::<H>(&app_context, name.as_ref(), &hash).await?;
        }
        Commands::Generate { component } => {
            let environment = Environment::from_str(&environment)
                .unwrap_or_else(|_| Environment::Any(environment.to_string()))
                .load()?;
            logger::init::<H>(&environment.logger);
            gen::generate::<H>(component.into(), &environment)?;
        }
        Commands::Doctor {} => {
            let environment = Environment::from_str(&environment)
                .unwrap_or_else(|_| Environment::Any(environment.to_string()))
                .load()?;
            let mut should_exit = false;
            for (_, check) in doctor::run_all(&environment).await {
                if !should_exit && !check.valid() {
                    should_exit = true;
                }
                println!("{check}");
            }
            if should_exit {
                exit(1);
            }
        }
        Commands::Version {} => {
            println!("{}", H::app_version(),);
        }
    }
    Ok(())
}

#[cfg(not(feature = "with-db"))]
pub async fn main<H: Hooks>() -> eyre::Result<()> {
    let cli = Cli::parse();
    let environment = cli
        .environment
        .or_else(resolve_from_env)
        .unwrap_or_else(|| DEFAULT_ENVIRONMENT.to_string());
    match cli.command {
        Commands::Start {
            worker,
            server_and_worker,
        } => {
            let start_mode = if worker {
                StartMode::WorkerOnly
            } else if server_and_worker {
                StartMode::ServerAndWorker
            } else {
                StartMode::ServerOnly
            };

            let boot_result = create_app::<H>(start_mode, &environment).await?;
            start(boot_result).await?;
        }
        Commands::Routes {} => show_list_endpoints::<H>(),
        Commands::Task { name, params } => {
            let mut hash = BTreeMap::new();
            for (k, v) in params {
                hash.insert(k, v);
            }
            let app_context = create_context::<H>(&environment).await?;
            run_task::<H>(&app_context, name.as_ref(), &hash).await?;
        }
        Commands::Generate { component } => {
            let environment = Environment::from_str(&environment)
                .unwrap_or_else(|_| Environment::Any(environment.to_string()))
                .load()?;
            logger::init::<H>(&environment.logger);
            gen::generate::<H>(component.into(), &environment)?;
        }
        Commands::Version {} => {
            println!("{}", H::app_version(),);
        }
    }
    Ok(())
}

fn show_list_endpoints<H: Hooks>() {
    let mut routes = list_endpoints::<H>();
    routes.sort_by(|a, b| a.uri.cmp(&b.uri));
    for router in routes {
        println!("{}", router.to_string());
    }
}
