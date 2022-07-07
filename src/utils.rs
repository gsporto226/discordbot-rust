use log::warn;
use serenity::{model::channel::Message, Result as SerenityResult};
pub fn handle_message_result(result: SerenityResult<Message>) {
    if let Err(why) = result {
        warn!("Error while trying to send message: {:?}", why);
    }
}