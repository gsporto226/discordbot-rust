use std::{sync::Arc, time::Instant};

use log::warn;
use parking_lot::Mutex;
use serenity::{model::channel::Message, Result as SerenityResult};

pub type ArcMut<T> = Arc<Mutex<T>>;

pub fn handle_message_result(result: SerenityResult<Message>) {
    if let Err(why) = result {
        warn!("Error while trying to send message: {:?}", why);
    }
}