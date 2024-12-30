# yt-rss-enhancer

This is a proxy that enhances YouTube RSS feeds in the following ways:
- Add the length of the video to the video title.
- Remove shorts.

## Setup

This guide assumes that you have Rust installed via rustup, and checked out the sources of this
repository into `~/src/yt-rss-enhancer`. To have systemd manage the proxy daemon, create a file
`~/.config/systemd/user/yt-rss-enhancer.service` with the following contents:

```
[Unit]
Description=Enhancing Proxy for YouTube RSS-feeds

[Service]
WorkingDirectory=%h/src/yt-rss-enhancer
ExecStartPre=%h/.cargo/bin/cargo build --release
ExecStart=%h/src/yt-rss-enhancer/target/release/yt-rss-enhancer state.json

[Install]
WantedBy=default.target
```

Then run
```
systemctl --user daemon-reload
systemctl --user enable yt-rss-enhancer
systemctl --user start yt-rss-enhancer
```

## Usage

For a channel with ID `UCYO_jab_esuFRV4b17AJtAw`, the feed URL would usually be `https://www.youtube.com/feeds/videos.xml?channel_id=UCYO_jab_esuFRV4b17AJtAw`.
To instead fetch the enhanced feed via this proxy use `http://127.0.0.1:12380/www.youtube.com/feeds/videos.xml?channel_id=UCYO_jab_esuFRV4b17AJtAw`.

The first time you fetch a feed, the reply will take a while since the proxy has to fetch the metadata for all the videos.
Your RSS reader will likely time out.
Just wait a minute or so, the metadata will be fetched in the background and cached.
Then try adding the feed again; thanks to the cache, the proxy should now reply quickly.
