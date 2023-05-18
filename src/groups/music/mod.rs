pub mod list;

use std::{time::Duration, collections::VecDeque};
use self::list::Shuffleable;

use super::{DiscordCommandError, GuildMusicResult};
use crate::{utils::{ArcMut, reply_with_result}, SONGBIRD};
use log::{debug, warn};
use parking_lot::Mutex;
use serenity::{
    async_trait,
    client::Context,
    framework::standard::{
        macros::{command, group},
        Args, CommandResult
    },
    model::{
        channel::Message,
        id::{ChannelId, GuildId, UserId}, guild,
    },
    prelude::{Mutex as AsyncMutex, TypeMapKey},
};
use songbird::{
    create_player,
    input::Restartable,
    tracks::{Track, TrackHandle},
    Call, Event, EventContext, EventHandler as VoiceEventHandler,
    TrackEvent,
    input::error::Error as SongbirdInputError
};
use url::Url;
use uuid::Uuid;
use std::{collections::HashMap, sync::Arc, time::Instant};

struct GuildMusicHashMapKey;
type TrackPair = (Track, TrackHandle);
type GuildMusicHashmap = HashMap<GuildId, Arc<GuildMusic>>;

impl TypeMapKey for GuildMusicHashMapKey {
    type Value = ArcMut<GuildMusicHashmap>;
}

lazy_static! {
    static ref GUILD_MUSIC_HASHMAP: ArcMut<GuildMusicHashmap> =
        Arc::new(Mutex::new(HashMap::new()));
}

#[derive(Debug)]
struct MusicRequest {
    uuid: uuid::Uuid,
    search_or_url: String,
    channel_id: ChannelId,
    _requested_by: UserId,
    _title: Option<String>,
    _author: Option<String>,
    _duration: Option<Duration>
}

#[derive(Debug)]
struct LoadedMusic {
    uuid: Uuid,
    track_pair: Option<TrackPair>
}

#[derive(Debug)]
struct NowPlaying {
    uuid: Uuid,
    track_handle: TrackHandle
}

#[derive(Debug)]
pub struct GuildMusicData {
    guild_id: GuildId,
    queue: VecDeque<MusicRequest>,
    preloaded: Option<LoadedMusic>,
    now_playing: Option<NowPlaying>,
    call_pair: Option<(ChannelId, Arc<AsyncMutex<Call>>)>,
}

#[allow(clippy::module_name_repetitions)]
pub struct GuildMusic {
    pub guild_music_data: Arc<Mutex<GuildMusicData>>,
}

async fn get_ytdl_track(search_or_url: String) -> Option<TrackPair> {
    let restartable = {
        if let Ok(url) = search_or_url.parse::<Url>() {
            Restartable::ytdl(url, false).await
        } else {
            Restartable::ytdl_search(search_or_url, false).await
        }
    };
    restartable.map(|restartable| create_player(restartable.into())).ok()
}

async fn get_call(guild_id: GuildId, channel_id: ChannelId) -> Option<Arc<AsyncMutex<Call>>> {
    let (call_lock, result) = SONGBIRD.join(guild_id, channel_id).await;
    match result {
        Ok(_channel) => {
            Some(call_lock)
        },
        Err(err) => {
            warn!("Error is {}, leave server {}, should reconnect {}", err, err.should_leave_server(), err.should_reconnect_driver());
            None
        }
    }
}

impl GuildMusic {
    pub fn new(guild_id: GuildId) -> Self {
        Self {
            guild_music_data: Arc::new(Mutex::new(GuildMusicData::new(guild_id))),
        }
    }

    fn tick_guild_music(&self) {
        let mut guild_music_data = self.guild_music_data.lock();
        if let Some((guild_id, next, track_pair_option, current_call_option)) = {
            if guild_music_data.now_playing.is_none() {
                if let Some(next) = guild_music_data.queue.pop_front() {
                    debug!("Next is {:?}", next);
                    let track_pair = guild_music_data.preloaded.take().and_then(|preloaded| {
                        preloaded.track_pair.and_then(|track_pair| {
                            if preloaded.uuid == next.uuid {
                                Some(track_pair)
                            } else {
                                None
                            }
                        })
                    });
                    let current_call = guild_music_data.call_pair.as_ref().and_then(|(channel_id, current_call_lock)| {
                        if channel_id.0 == next.channel_id.0 {
                            Some(current_call_lock.clone())
                        } else {
                            None
                        }
                    });
                    Some((guild_music_data.guild_id, next, track_pair, current_call))
                } else {
                    None
                }
            } else {
                None
            }
        } {
            let play_guild_music_data_lock = self.guild_music_data.clone();
            tokio::spawn(async move {
                match {
                    if track_pair_option.is_some() {
                        track_pair_option
                    } else {
                        get_ytdl_track(next.search_or_url).await
                    }
                } {
                    Some((track, track_handle)) => {
                        match {
                            if current_call_option.is_some() {
                                current_call_option
                            } else {
                                get_call(guild_id, next.channel_id).await
                            }
                        } {
                            Some(call_lock) => {
                                {
                                    play_guild_music_data_lock.lock().now_playing = Some(NowPlaying { uuid: next.uuid, track_handle });
                                }
                                let elapsed = Instant::now();
                                let mut call = call_lock.lock().await;
                                debug!("Spent {}ms waiting for call lock", elapsed.elapsed().as_millis());
                                call.play(track);
                                call.remove_all_global_events();
                                call.add_global_event(
                                    Event::Track(TrackEvent::End),
                                    TrackEventListener { guild_id, music_uuid: next.uuid }
                                );
                            }
                            None => {
                                warn!("Failed to get voice channel");
                                // reply with failure to get voice channel
                            }
                        };
                    }
                    None => {
                        warn!("Failed to get track");
                        // reply with failure
                    }
                }
            });
        };
        let music_to_preload = guild_music_data.queue.get(0).map(|music_request| {
            (music_request.uuid, music_request.search_or_url.clone())
        });
        if let Some((uuid, _url)) = &music_to_preload {
            guild_music_data.preloaded = Some(LoadedMusic { uuid: *uuid, track_pair: None });
        };
        if let Some((uuid, url)) = music_to_preload {
            let preload_guild_music_data_lock = self.guild_music_data.clone();
            tokio::spawn(async move {
                debug!("Preloading for uuid {} and url {}", uuid, url);
                let time_elapsed = Instant::now();
                if let Some(track_pair) = get_ytdl_track(url).await {
                    debug!("Finished preloading for uuid {} in {}ms", uuid, time_elapsed.elapsed().as_millis());
                    if let Some(ref mut preloaded) = preload_guild_music_data_lock.lock().preloaded {
                        if preloaded.uuid == uuid {
                            preloaded.track_pair = Some(track_pair);
                        };
                    };
                };
            });
        };
    }

    pub fn insert_music(&self, search_or_url: String, channel_id: ChannelId, requested_by: UserId) {
        {
            let mut guild_music_data = self.guild_music_data.lock();
            guild_music_data.queue.push_back(MusicRequest {
                uuid: Uuid::new_v4(),
                search_or_url,
                channel_id,
                _requested_by: requested_by,
                _author: None,
                _duration: None,
                _title: None
            });
            debug!("Inserted music into guild {:?}", guild_music_data.guild_id);
        }
        self.tick_guild_music();
    }

    pub fn handle_track_event(&self, _ctx: &EventContext<'_>, music_uuid: Uuid) {
        {
            let mut guild_music_data = self.guild_music_data.lock();
            if let Some(now_playing) = &guild_music_data.now_playing {
                if now_playing.uuid == music_uuid {
                    if now_playing.track_handle.stop().is_err() {};
                    guild_music_data.now_playing = None;
                };
            }
        }
        self.tick_guild_music();
    }

    pub fn stop(&self) -> GuildMusicResult<()> {
        let mut guild_music_data = self.guild_music_data.lock();
        if let Some(now_playing) = guild_music_data.now_playing.take() {
            guild_music_data.queue = VecDeque::new();
            if let Err(why) = now_playing.track_handle.stop() {
                return Err(DiscordCommandError { source: Some(Box::new(why)), severity: super::ErrorSeverity::Internal, kind: super::ErrorKind::InternalError })
            };
            Ok(())
        } else {
            Err(DiscordCommandError { source: None, severity: super::ErrorSeverity::UserInput, kind: super::ErrorKind::NoSongCurrentlyPlaying})
        }
    }

    // maybe clean and or improve this logic up a bit
    pub fn skip(&self, quantity: usize) -> GuildMusicResult<usize> {
        let mut guild_music_data = self.guild_music_data.lock();
        let queue_len = guild_music_data.queue.len();
        let removed_from_queue = { 
            let mut to_remove_from_queue = 0;
            if quantity > 1 && queue_len > 0 {
                to_remove_from_queue = {
                    if quantity - 1 > guild_music_data.queue.len() {
                        guild_music_data.queue.len()
                    } else {
                        quantity - 1
                    }
                };
                guild_music_data.queue = guild_music_data.queue.split_off(to_remove_from_queue);
            };
            to_remove_from_queue
        };
        if let Some(now_playing) = &guild_music_data.now_playing {
            if let Err(why) = now_playing.track_handle.stop() {
                Err( DiscordCommandError {
                    kind: super::ErrorKind::InternalError,
                    source: Some(Box::new(why)),
                    severity: super::ErrorSeverity::Internal
                })
            } else {
                Ok(removed_from_queue + 1)
            }
        } else {
            Err( DiscordCommandError { 
                kind: super::ErrorKind::NoSongCurrentlyPlaying,
                source: None,
                severity: super::ErrorSeverity::UserInput
            })
        }
    }

    pub fn shuffle(&self) -> GuildMusicResult<()> {
        let mut guild_music_data = self.guild_music_data.lock();
        if guild_music_data.queue.is_empty() {
            Err(DiscordCommandError { source: None, severity: super::ErrorSeverity::UserInput, kind: super::ErrorKind::QueueIsEmpty })
        } else {
            guild_music_data.queue.shuffle();
            Ok(())
        }
    }
}

impl GuildMusicData {
    pub(crate) fn new(guild_id: GuildId) -> Self {
        Self {
            guild_id,
            queue: VecDeque::new(),
            preloaded: None,
            now_playing: None,
            call_pair: None,
        }
    }
}

#[group]
#[description = "music_group_description"]
#[only_in("guilds")]
#[commands(play, skip, stop, shuffle)]
pub struct Music;

struct TrackEventListener {
    guild_id: GuildId,
    music_uuid: Uuid
}

#[async_trait]
impl VoiceEventHandler for TrackEventListener {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let Some(guild_music) = GUILD_MUSIC_HASHMAP.lock().get(&self.guild_id) {
            guild_music.handle_track_event(ctx, self.music_uuid);
        };
        None
    }
}

async fn get_guild_music_for(context: &Context, guild_id: GuildId) -> Arc<GuildMusic> {
    let mut data = context.data.write().await;
    let mut guild_music_hashmap = GUILD_MUSIC_HASHMAP.lock();
    let guild_music = guild_music_hashmap
        .entry(guild_id)
        .or_insert_with(|| Arc::new(GuildMusic::new(guild_id)));
    if !data.contains_key::<GuildMusicHashMapKey>() {
        data.insert::<GuildMusicHashMapKey>(GUILD_MUSIC_HASHMAP.clone());
    };
    guild_music.clone()
}

async fn get_voice_guild_and_channel(
    context: &Context,
    message: &Message,
) -> Option<(GuildId, ChannelId)> {
    if let Some(guild) = message.guild(&context.cache) {
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

#[command]
#[aliases("p", "ply")]
async fn play(context: &Context, message: &Message, mut args: Args) -> CommandResult {
    let requested_by = message.author.id;
    match get_voice_guild_and_channel(context, message).await {
        Some((guild_id, channel_id)) => {
            match args.single::<String>() {
                Ok(argument) => {
                    get_guild_music_for(context, guild_id).await.insert_music(
                        argument,
                        channel_id,
                        requested_by,
                    );
                    Ok(())
                },
                Err(_) =>  {
                    Err(DiscordCommandError {
                        source: None,
                        severity: super::ErrorSeverity::UserInput,
                        kind: super::ErrorKind::MustProvideSomeArguments(1),
                    }
                    .into())
                },
            }
        },
        None => Err(DiscordCommandError {
            source: None,
            severity: super::ErrorSeverity::UserInput,
            kind: super::ErrorKind::NotInVoiceChannel,
        }
        .into()),
    }
}

#[command]
#[aliases("stp")]
async fn stop(context: &Context, message: &Message, mut _args: Args) -> CommandResult {
    if let Some(guild_id) = message.guild_id {
        if let Err(err) = get_guild_music_for(context, guild_id).await.stop() {
            Err(err.into())
        } else {
            reply_with_result(context, message, "Stopped current song and cleared the queue!".to_string(), false).await;
            Ok(())
        }
    } else {
        Err(DiscordCommandError {
            source: None,
            severity: super::ErrorSeverity::UserInput,
            kind: super::ErrorKind::MessageNotInGuildChannel,
        }
        .into())
    }
}

#[command]
#[aliases("s", "skp")]
async fn skip(context: &Context, message: &Message, mut args: Args) -> CommandResult {
    if let Some(guild_id) = message.guild_id {
        let quantity = args.single::<usize>().unwrap_or(1);
        match get_guild_music_for(context, guild_id).await.skip(quantity) {
            Ok(removed) => {
                reply_with_result(context, message, format!("Successfully skipped {} song(s)!", removed), false).await;
                Ok(())
            },
            Err(err) => Err(err.into()),
        }
    } else {
        Err(DiscordCommandError {
            source: None,
            severity: super::ErrorSeverity::UserInput,
            kind: super::ErrorKind::MessageNotInGuildChannel,
        }
        .into())
    }
}

#[command]
#[aliases("scramble", "randomize", "random")]
async fn shuffle(context: &Context, message: &Message, mut _args: Args) -> CommandResult {
    if let Some(guild_id) = message.guild_id {
        if let Err(err) = get_guild_music_for(context, guild_id).await.shuffle() {
            Err(err.into())
        } else {
            reply_with_result(context, message, "Successfully shuffled queue!".to_string(), false).await;
            Ok(())
        }
    } else {
        Err(DiscordCommandError {
            source: None,
            severity: super::ErrorSeverity::UserInput,
            kind: super::ErrorKind::MessageNotInGuildChannel,
        }
        .into())
    }
}