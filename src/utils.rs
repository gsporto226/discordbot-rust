use std::{sync::Arc};

use log::warn;
use parking_lot::Mutex;
use serenity::{model::channel::Message, Result as SerenityResult, client::Context, prelude::ModelError, Error as SerenityError};

pub type ArcMut<T> = Arc<Mutex<T>>;

pub fn handle_message_result(result: SerenityResult<Message>) {
    if let Err(why) = result {
        warn!("Error while trying to send message: {:?}", why);
    }
}

pub async fn reply_with_result(context: &Context, message: &Message, result: String, private: bool) {
    if private {
        if let Ok(dm_channel) = message.author.id.create_dm_channel(context).await {
            if let Err(why) = dm_channel.send_message(context, |create_message| create_message.content(result.as_str())).await {
                warn!("Errored trying to send message {} with error: {}", result.as_str(), why);
            };
        };
    } else {
        match message.reply(context, result.as_str()).await {
            // Err(SerenityError::Model(ModelError::InvalidPermissions(permissions))) => {
            //     if permissions.send_messages() && !permissions.read_message_history() {
            //         if let Ok(channel) = message.channel(context.cache.clone()).await {
            //             if let Err(why) = channel.id().send_message(context, |create_message| create_message.content(result.as_str())).await {
            //                 warn!("Errored trying to send message {} with error: {}", result.as_str(), why);
            //             };
            //         };
            //     };
            // },
            Err(SerenityError::Model(ModelError::MessageTooLong(over))) => {
                warn!("Tried to send message that was too long by {} characters: {}", over, result.as_str());
            },
            Err(why) => {
                warn!("Unexpected error while trying to reply to user: {}", why);
            }
            Ok(_) => {},
        };
    };
}