use std::{collections::HashMap, sync::Arc};

use log::{debug, warn};
use serenity::{
    async_trait,
    client::Context,
    framework::standard::{
        buckets::RevertBucket,
        macros::{check, command, group},
        Args, CommandError, CommandOptions, CommandResult, Reason,
    },
    model::{
        channel::Message,
        guild,
        id::{ChannelId, GuildId, UserId},
    },
    prelude::{Mutex, TypeMapKey},
};
use songbird::{
    create_player,
    input::Restartable,
    tracks::{Track, TrackHandle},
    Call, Event, EventContext, EventHandler as VoiceEventHandler, Songbird, SongbirdKey,
    TrackEvent,
};

use crate::SONGBIRD;

use super::DiscordCommandError;

struct GuildMusicHashMapKey;

impl TypeMapKey for GuildMusicHashMapKey {
    type Value = HashMap<GuildId, Arc<Mutex<GuildMusicData>>>;
}

#[derive(Debug)]
struct MusicQueueEntry {
    url: String,
    channel_id: ChannelId,
    requested_by: UserId,
}

#[derive(Debug)]
pub(crate) struct GuildMusicData {
    guild_id: GuildId,
    queue: Vec<MusicQueueEntry>,
    preloaded: HashMap<String, (Track, TrackHandle)>,
    now_playing: Option<TrackHandle>,
    call_pair: Option<(ChannelId, Arc<Mutex<Call>>)>,
}

impl GuildMusicData {
    pub(crate) fn new(guild_id: GuildId) -> Self {
        Self {
            guild_id,
            queue: vec![],
            preloaded: HashMap::new(),
            now_playing: None,
            call_pair: None,
        }
    }
}

#[group]
#[description = "music_group_description"]
#[only_in("guilds")]
#[commands(play, skip)]
pub struct Music;

async fn get_guild_music_data(
    context: &Context,
    guild_id: GuildId,
) -> Arc<Mutex<GuildMusicData>> {
    let mut data = context.data.write().await;
    let guild_data = GuildMusicData::new(guild_id);
    match data.get_mut::<GuildMusicHashMapKey>() {
        Some(hashmap) => {
            let guild_music_data = hashmap
                .entry(guild_id)
                .or_insert(Arc::new(Mutex::new(guild_data)));
            guild_music_data.clone()
        }
        None => {
            let mut map: HashMap<GuildId, Arc<Mutex<GuildMusicData>>> = HashMap::new();
            let guild_music_data = Arc::new(Mutex::new(guild_data));
            map.insert(guild_id, guild_music_data.clone());
            data.insert::<GuildMusicHashMapKey>(map);
            guild_music_data
        }
    }
}

async fn get_voice_guild_and_channel(
    context: &Context,
    message: &Message,
) -> Option<(GuildId, ChannelId)> {
    if let Some(guild) = message.guild(&context.cache).await {
        let guild_id = guild.id;
        let channel_id = guild
            .voice_states
            .get(&message.author.id)
            .and_then(|voice_state| voice_state.channel_id);
        if let Some(channel_id) = channel_id {
            return Some((guild_id, channel_id));
        };
    }
    None
}

fn queue_is_not_empty(guild_music_data: GuildMusicData) -> bool {
    todo!();
}

async fn get_ytdl_track(url: String) -> Option<(Track, TrackHandle)> {
    match Restartable::ytdl(url, false).await {
        Ok(restartable) => Some(create_player(restartable.into())),
        Err(_error) => None,
    }
}

struct TrackEndListener {
    guild_music_data_lock: Arc<Mutex<GuildMusicData>>,
}

#[async_trait]
impl VoiceEventHandler for TrackEndListener {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        {
            let mut guild_music_data = self.guild_music_data_lock.lock().await;
            guild_music_data.now_playing = None;
        }
        tick_guild_music(self.guild_music_data_lock.clone()).await;
        None
    }
}

async fn get_call(guild_id: GuildId, channel_id: ChannelId) -> Option<Arc<Mutex<Call>>> {
    let (call_lock, result) = SONGBIRD.join(guild_id, channel_id).await;
    if let Ok(_channel) = result {
        Some(call_lock)
    } else {
        None
    }
}

async fn tick_guild_music(guild_music_data_lock: Arc<Mutex<GuildMusicData>>) {
    let mut guild_music_data = guild_music_data_lock.lock().await;
    if guild_music_data.now_playing.is_none() {
        debug!("Now playing is none, getting next for guild {:?}", guild_music_data.guild_id);
        if let Some(next) = guild_music_data.queue.pop() {
            debug!("Will try to play {:?}", next);
            if let Some((track, handle)) = {
                if let Some((_url, result)) = guild_music_data.preloaded.remove_entry(&next.url) {
                    Some(result)
                } else {
                    get_ytdl_track(next.url).await
                }
            } {
                debug!("Found track and handle");
                if let Some(call_lock) = {
                    if let Some((channel_id, current_call_lock)) = &guild_music_data.call_pair {
                        if channel_id.0 == next.channel_id.0 {
                            Some(current_call_lock.clone())
                        } else {
                            get_call(guild_music_data.guild_id, next.channel_id).await
                        }
                    } else {
                        get_call(guild_music_data.guild_id, next.channel_id).await
                    }
                } {
                    let mut call = call_lock.lock().await;
                    debug!("Acquired call lock");
                    guild_music_data.now_playing = Some(handle);
                    call.play(track);
                    call.remove_all_global_events();
                    call.add_global_event(
                        Event::Track(TrackEvent::End),
                        TrackEndListener {
                            guild_music_data_lock: guild_music_data_lock.clone(),
                        },
                    );
                }
            }
        }
    }
}

async fn insert_music_into(
    guild_music_data_lock: Arc<Mutex<GuildMusicData>>,
    url: String,
    channel_id: ChannelId,
    requested_by: UserId,
) {
    {
        let mut guild_music_data = guild_music_data_lock.lock().await;
        guild_music_data.queue.push(MusicQueueEntry {
            url,
            channel_id,
            requested_by,
        });
        debug!("Inserted music into guild {:?}", guild_music_data.guild_id);
    }
    tick_guild_music(guild_music_data_lock.clone()).await;
}

#[command]
#[aliases("p", "ply")]
async fn play(context: &Context, message: &Message, mut args: Args) -> CommandResult {
    let user_id = message.author.id;
    match get_voice_guild_and_channel(context, message).await {
        Some((guild_id, channel_id)) => {
            if let Ok(url) = args.single::<String>() {
                insert_music_into(get_guild_music_data(context, guild_id).await, url, channel_id, user_id).await;
                Ok(())
            } else {
                Err(DiscordCommandError {
                    source: None,
                    severity: super::ErrorSeverity::UserInput,
                    kind: super::ErrorKind::MustProvideURL,
                }
                .into())
            }
        }
        None => Err(DiscordCommandError {
            source: None,
            severity: super::ErrorSeverity::UserInput,
            kind: super::ErrorKind::NotInVoiceChannel,
        }
        .into()),
    }
}

// async fn join(manager: Arc<Songbird>, guild_id: GuildId, channel_id: ChannelId) -> Result<Arc<Mutex<Call>>, DiscordCommandError> {
//     let (call, result) = manager.join(guild_id, channel_id).await;
//     if let Err(error) = result {
//         return Err(DiscordCommandError { kind: super::ErrorKind::InternalError, severity: super::ErrorSeverity::Internal, source: Some(Box::new(error)) });
//     };
//     Ok(call)
// }
// #[command]
// async fn stop() {}
#[command]
#[aliases("s", "skp")]
async fn skip(context: &Context, message: &Message, mut _args: Args) -> CommandResult {
    if let Some(guild_id) = message.guild_id {
        let guild_music_data_lock = get_guild_music_data(context, guild_id).await;
        let guild_music_data = guild_music_data_lock.lock().await;
        if let Some(now_playing) = &guild_music_data.now_playing {
            if let Err(why) = now_playing.stop() {
                Err(DiscordCommandError { source: Some(Box::new(why)), severity: super::ErrorSeverity::Internal, kind: super::ErrorKind::InternalError }.into())
            } else {
                Ok(())
            }
        } else {
            Err(DiscordCommandError { source: None, severity: super::ErrorSeverity::UserInput, kind: super::ErrorKind::NoSongCurrentlyPlaying }.into())
        }
    } else {
        Err(DiscordCommandError { source: None, severity: super::ErrorSeverity::UserInput, kind: super::ErrorKind::MessageNotInGuildChannel }.into())
    }
}
// #[command]
// async fn queue() {}
// async fn scramble() {}
// async fn remove() {}
// async fn history() {}
// async fn favorites() {}
