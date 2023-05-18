#![warn(clippy::pedantic)]

#[macro_use]
extern crate lazy_static;

use std::{env, sync::Arc};
use groups::{DiscordCommandError};
use log::{info, warn, debug};
use serenity::{client::{EventHandler, Context}, async_trait, Client, framework::{StandardFramework, standard::{macros::{group, command, hook}, CommandResult, Command, buckets::RevertBucket}}, model::channel::Message, prelude::GatewayIntents};

mod constants;
mod structures;
mod groups;
mod utils;
pub use constants::*;
use songbird::Songbird;

use crate::{groups::music::MUSIC_GROUP, utils::reply_with_result};

lazy_static! {
    static ref SONGBIRD: Arc<Songbird> = Songbird::serenity();
}

struct Handler;

#[async_trait]
impl EventHandler for Handler {}

#[hook]
async fn before(_context: &Context, message: &Message, command_name: &str) -> bool {
    debug!("Got command {} by user {}", command_name, message.author.name);
    true
}

#[hook]
async fn after(context: &Context, message: &Message, command_name: &str, command_result: CommandResult) {
    if let Err(err) = command_result {
        match err.downcast::<DiscordCommandError>() {
            Ok(error) => {
                info!("Error is {}", error);
                reply_with_result(context, message, format!("{error}"), false).await;
            },
            Err(why) => {
                warn!("Returned error from command '{}' is not DiscordCommandError: {}", command_name, why);
            }
        }
    };
}

#[tokio::main]
async fn main() {
    info!("CLIENT STARTING ==============");
    let token = env::var(TOKEN_ENVIRONMENT_VARIABLE).expect("Expected BOT_RUST_TOKEN to be set as a environment variable");
    let intents = GatewayIntents::all();
    let framework = StandardFramework::new()
        .configure(|c| c
            .with_whitespace(true)
            .prefix("!")
        )
        .before(before)
        .after(after)
        .group(&MUSIC_GROUP);
    let mut client = Client::builder(&token, intents)
        .event_handler(Handler)
        .framework(framework)
        .voice_manager_arc(SONGBIRD.clone())
        .await
        .expect("Error creating client");
    match client.start_autosharded().await {
        Ok(_) => {
            info!("Client started!");
        },
        Err(error) => {
            warn!("Client failed to start with error: {}", error);
        }
    }
}

fn setup_logging() {
    let stdout_appender = log4rs::append::console::ConsoleAppender::builder()
        .encoder(Box::new(log4rs::encode::pattern::PatternEncoder::new("{h({l} - {d(%Y-%m-%d %H:%M:%S %Z)(local)} - {m}{n})}")))
        .build();
    let logfile_appender_result = log4rs::append::file::FileAppender::builder()
        .encoder(Box::new(log4rs::encode::pattern::PatternEncoder::new("{d} - {m}{n}")))
        .build(LOG_FILE_PATH);
    let mut config_builder = log4rs::Config::builder()
        .appender(log4rs::config::Appender::builder()
            .filter(Box::new(log4rs::filter::threshold::ThresholdFilter::new(STDOUT_LOG_LEVEL)))
            .build("stdout", Box::new(stdout_appender))
        )
        .logger(log4rs::config::Logger::builder().build("lib::websocket::stdout", log::LevelFilter::Info));
    match logfile_appender_result {
        Ok(logfile_appender) => {
            config_builder = config_builder
            .appender(log4rs::config::Appender::builder().build("file", Box::new(logfile_appender)))
            .logger(log4rs::config::Logger::builder()
                .appender("file")
                .additive(false)
                .build("lib::websocket::file", log::LevelFilter::Trace)
            );
        },
        Err(error) => {
            eprintln!("Failed to initialize file logger with error: {error}");
        }
    }
    match config_builder.build(log4rs::config::Root::builder()
            .appender("stdout")
            .appender("file")
            .build(log::LevelFilter::Trace)
        ) {
        Ok(config) => {
            if let Err(error) = log4rs::init_config(config) {
                eprintln!("Failed to initialize logger with error: {error}");
            }
        },
        Err(error) => eprintln!("Failed to build logger configuration with error: {error}"),
    }
}