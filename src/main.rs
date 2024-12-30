use std::io::{BufReader, BufWriter};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::{collections::HashMap, fs::File};
use std::{env, fs};

use anyhow::{anyhow, bail, Context, Result};
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
}

#[derive(Serialize, Deserialize, Default)]
struct State {
    youtube_videos: HashMap<VideoId, YoutubeVideo>,
    /// Whether the state changed and should be written back to persistent storage soon.
    #[serde(skip)]
    dirty: bool,
    /// The filename where the state is stored.
    #[serde(skip)]
    file: String,
}

fn load_state(state_file: String) -> Result<State> {
    let mut state = if fs::exists(&state_file).unwrap_or(false) {
        let f = File::open(&state_file)?;
        serde_json::from_reader(BufReader::new(f))?
    } else {
        State::default()
    };
    state.file = state_file;
    Ok(state)
}

fn store_state(state: &Arc<Mutex<State>>) -> Result<()> {
    // We deliberately hold the lock around the entire thing, so that no two
    // threads try to write the same file at the same time.
    let state = state.lock().unwrap();
    if !state.dirty {
        // Nothing to do.
        return Ok(());
    }
    let f = File::create(&state.file)?;
    serde_json::to_writer_pretty(BufWriter::new(f), &*state)?;
    Ok(())
}

fn fetch_youtube_video_data(video_id: &str) -> Result<YoutubeVideo> {
    #[derive(Deserialize)]
    struct YtDlpJson {
        duration: u64,
        width: u64,
        height: u64,
    }

    // Run yt-dlp and parse the JSON it produces.
    let mut child = Command::new("yt-dlp")
        .arg("--dump-json")
        .arg(format!("https://www.youtube.com/watch?v={video_id}"))
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to start yt-dlp; make sure it is installed")?;
    let json: YtDlpJson = serde_json::from_reader(child.stdout.take().unwrap())
        .context("failed to parse yt-dlp JSON")?;
    if !child.wait()?.success() {
        bail!("yt-dlp returned non-zero exit status");
    }

    // Convert the yt-dlp output into our own format.
    let is_short = json.duration <= 180 && json.height >= json.width;
    Ok(YoutubeVideo {
        timestamp: Utc::now(),
        length: json.duration,
        is_short,
    })
}

fn get_youtube_video_data(state: &Arc<Mutex<State>>, video_id: &str) -> Result<YoutubeVideo> {
    // Check if we already have the video cached.
    if let Some(video) = state.lock().unwrap().youtube_videos.get(video_id).cloned() {
        // We assume that size and length of the video generally don't change,
        // so we can use the cached data.
        return Ok(video);
    }

    let video_data = fetch_youtube_video_data(video_id)?;

    let mut state = state.lock().unwrap();
    state
        .youtube_videos
        .insert(video_id.to_owned(), video_data.clone());
    state.dirty = true;
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
        .send()
        .context("failed to fetch RSS feed from YouTube")?;
    let mut feed =
        xmltree::Element::parse(feed_xml).context("failed to parse RSS feed from YouTube")?;

    // Take all the entries from the feed, and collect (some of) them in modified form.
    let mut entries = vec![];
    while let Some(mut entry) = feed.take_child("entry") {
        // Get the video metadata.
        let video_id = entry
            .get_child("videoId")
            .and_then(|e| e.get_text())
            .ok_or_else(|| anyhow!("videoId element missing"))?;
        let title = entry
            .get_child("title")
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
            duration = format_duration(video_data.length)
        );
        let title_elem = entry
            .get_mut_child("title")
            .ok_or_else(|| anyhow!("title element missing"))?;
        title_elem.children = vec![xmltree::XMLNode::Text(title)];

        // Remove "updated" so that the videos keep their original dates.
        // (Thunderbird displays the "updated" date instead of the "published" one.)
        while let Some(_) = entry.take_child("updated") {}

        entries.push(entry);
    }
    // Add the transformed elements back.
    for entry in entries {
        feed.children.push(xmltree::XMLNode::Element(entry));
    }

    // Store cached state.
    store_state(state).context("failed to store persistent state")?;

    // Turn this into XML again.
    let mut output: Vec<u8> = vec![];
    feed.write_with_config(
        &mut output,
        xmltree::EmitterConfig {
            perform_indent: true,
            ..Default::default()
        },
    )
    .context("failed to serialize adjusted RSS feed")?;
    output.push(b'\n'); // trailing newline is nice for testing

    Ok(Response::from_data("text/xml", output))
}

fn main() -> Result<()> {
    let state_file = env::args()
        .nth(1)
        .ok_or_else(|| anyhow!("state file name must be passed as first argument"))?;
    let state = Arc::new(Mutex::new(
        load_state(state_file).context("failed to load persistent state")?,
    ));
    rouille::start_server("127.0.0.1:12380", move |request: &Request| {
        let response = match &*request.url() {
            "/www.youtube.com/feeds/videos.xml" => handle_youtube_feed(&state, request),
            url => Ok(Response::text(format!("endpoint not found: {url}\n")).with_status_code(404)),
        };
        response.unwrap_or_else(|err| {
            eprintln!("error handling {}: {err}", request.url());
            Response::text(format!("internal server error: {err}\n")).with_status_code(500)
        })
    });
}
