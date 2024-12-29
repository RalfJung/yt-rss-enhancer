use std::fs;
use std::io::{BufReader, BufWriter};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::{collections::HashMap, fs::File};

use anyhow::{anyhow, bail, Result};
use chrono::prelude::*;
use rouille::{Request, Response};
use serde_derive::{Deserialize, Serialize};

type VideoId = String;

#[derive(Serialize, Deserialize, Clone, Debug)]
struct YoutubeVideo {
    /// Timestamp of this information.
    #[serde(with = "chrono::serde::ts_seconds")]
    timestamp: DateTime<Utc>,
    /// Length in seconds.
    length: u64,
    /// Is this a short?
    is_short: bool,
    /// Video title.
    title: String,
}

#[derive(Serialize, Deserialize, Default)]
struct State {
    youtube_videos: HashMap<VideoId, YoutubeVideo>,
}

const STATE_FILE: &str = "state.json";

fn load_state() -> Result<State> {
    Ok(if fs::exists(STATE_FILE).unwrap_or(false) {
        let f = File::open(STATE_FILE)?;
        serde_json::from_reader(BufReader::new(f))?
    } else {
        State::default()
    })
}

fn store_state(state: &Arc<Mutex<State>>) -> Result<()> {
    // We deliberately hold the lock around the entire thing, so that no two
    // threads try to write the same file at the same time.
    let state = state.lock().unwrap();
    let f = File::create(STATE_FILE)?;
    serde_json::to_writer_pretty(BufWriter::new(f), &*state)?;
    Ok(())
}

fn fetch_youtube_video_data(video_id: &str) -> Result<YoutubeVideo> {
    #[derive(Deserialize)]
    struct YtDlpJson {
        duration: u64,
        title: String,
        width: u64,
        height: u64,
    }

    // Run yt-dlp and parse the JSON it produces.
    let mut child = Command::new("yt-dlp")
        .arg("--dump-json")
        .arg(format!("https://www.youtube.com/watch?v={video_id}"))
        .stdout(Stdio::piped())
        .spawn()?;
    let json: YtDlpJson = serde_json::from_reader(child.stdout.take().unwrap())?;
    if !child.wait()?.success() {
        bail!("yt-dlp returned non-zero exit status");
    }

    // Convert the yt-dlp output into our own format.
    let is_short = json.duration <= 180 && json.height >= json.width;
    Ok(YoutubeVideo {
        timestamp: Utc::now(),
        length: json.duration,
        is_short,
        title: json.title,
    })
}

fn get_youtube_video_data(state: &Arc<Mutex<State>>, video_id: &str) -> Result<YoutubeVideo> {
    // Check if we already have the video cached.
    // TODO: check if the cache is reasonably up-to-date.
    if let Some(video) = state.lock().unwrap().youtube_videos.get(video_id).cloned() {
        return Ok(video);
    }

    let video_data = fetch_youtube_video_data(video_id)?;
    state
        .lock()
        .unwrap()
        .youtube_videos
        .insert(video_id.to_owned(), video_data.clone());
    Ok(video_data)
}

fn format_duration(seconds: u64) -> String {
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    if minutes == 0 {
        format!("{seconds}s")
    } else {
        format!("{minutes}:{seconds:02}s")
    }
}

fn handle_youtube_feed(state: &Arc<Mutex<State>>, request: &Request) -> Result<Response> {
    let feed_id = request
        .get_param("channel_id")
        .ok_or_else(|| anyhow!("channel_id param missing"))?;

    // Fetch feed from youtube.
    let feed_xml = attohttpc::get("https://www.youtube.com/feeds/videos.xml")
        .param("channel_id", &feed_id)
        .send()?;
    let mut feed = xmltree::Element::parse(feed_xml)?;

    // Take all the entries from the feed, and collect (some of) them in modified form.
    let mut entries = vec![];
    while let Some(mut entry) = feed.take_child("entry") {
        // Get the video metadata.
        let video_id = entry
            .get_child("videoId")
            .and_then(|e| e.get_text())
            .ok_or_else(|| anyhow!("videoId element missing"))?;
        let video_data = get_youtube_video_data(state, &video_id)?;

        // Skip shorts.
        if video_data.is_short {
            continue;
        }

        // Update title.
        let title = format!(
            "{title} ({duration})",
            title = video_data.title,
            duration = format_duration(video_data.length)
        );
        let title_elem = entry
            .get_mut_child("title")
            .ok_or_else(|| anyhow!("title element missing"))?;
        title_elem.children = vec![xmltree::XMLNode::Text(title)];

        // Remove the "media:group" thing, Thunderbird ignores it anyway.
        while let Some(_) = entry.take_child("group") {}
        // Also remove "updated" so that the videos keep their original dates.
        while let Some(_) = entry.take_child("updated") {}

        entries.push(entry);
    }
    // Add the transformed elements back.
    for entry in entries {
        feed.children.push(xmltree::XMLNode::Element(entry));
    }

    // Store cached state.
    store_state(state)?;

    // Turn this into XML again.
    let mut output: Vec<u8> = vec![];
    feed.write_with_config(
        &mut output,
        xmltree::EmitterConfig {
            perform_indent: true,
            ..Default::default()
        },
    )?;

    Ok(Response::from_data("text/xml", output))
}

fn main() -> Result<()> {
    let state = Arc::new(Mutex::new(load_state()?));
    rouille::start_server("127.0.0.1:12380", move |request: &Request| {
        let response = match &*request.url() {
            "/www.youtube.com/feeds/videos.xml" => handle_youtube_feed(&state, request),
            url => Ok(Response::text(format!("endpoint not found: {url}")).with_status_code(404)),
        };
        response.unwrap_or_else(|err| {
            Response::text(format!("internal server error: {err}")).with_status_code(500)
        })
    });
}
