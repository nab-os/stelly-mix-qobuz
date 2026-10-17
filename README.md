# stelly-mix-qobuz

[stelly-mix](https://github.com/nab-os/stelly-mix) with Qobuz as a source.
Private, and separate, so that stelly-mix carries nothing of Qobuz's: this
crate registers a `Source` and builds its own binary, which is stelly-mix
with `qobuz:` references wherever it takes a track.

Qobuz itself is [stelly-qobuz](https://github.com/nab-os/stelly-qobuz)'s, the
gateway Stelly uses too: it holds the account's credentials and its one
request queue, so this and Stelly spend one 2/s budget between them, an
index queued behind Stelly's browsing.

```sh
stelly-qobuz serve &                                        # once, if not already up
cargo run --release -- search qobuz "remon verhoeve"
cargo run --release -- index qobuz:favourites
cargo run --release -- set --like "chill downtempo" --out chill.wav
```

References: `qobuz:<track id>`, `qobuz:favourites`, `qobuz:playlist/<id>`,
`qobuz:album/<id>`. Tracks are fetched once, FLAC at CD quality
(`QOBUZ_FORMAT=mp3` for MP3 320; a track with no lossless master comes as
MP3 either way), into `~/.cache/stelly-mix/qobuz`, and analysed and mixed
from there. That cache is the library's audio, so nothing evicts it: at CD
quality a track is ~30MB. A collection brings in up to 2000 tracks
(`QOBUZ_LIMIT` for more), and says so when it is cut; a track Qobuz will not
stream in full, delisted or preview-only, is reported and passed over.
The downloads come from Qobuz's CDN, outside the account's budget.

## Stelly's space

`stelly:` references are tracks Stelly's server chooses from its space, the
tens of thousands it has analysed with the same CLAP model, fetched from
Qobuz like `qobuz:` ones and into the same cache:

```sh
cargo run --release -- index "stelly:mood/dark rolling techno?n=200"
cargo run --release -- search stelly "artist or title"          # ids for the rest
cargo run --release -- index stelly:radio/<id>                    # a walk from a track
cargo run --release -- index stelly:neighbours/<id>?n=50          # the nearest to one
cargo run --release -- index stelly:drift/<id>/<description>      # from one towards a mood
cargo run --release -- index stelly:path/<id>/<id>                # from one track to another
```

It wants Stelly's server and a device token for it, `stelly-server pair
--name stelly-mix --scope play --user <you>`: `STELLY_SERVER` and
`STELLY_TOKEN`, in the environment or `~/.config/stelly-mix/qobuz.env`. The
space holds what Stelly grew from its likes: a genre it never crawled is not
in it, whatever the description.

## The window

```sh
cargo run --release --features app --bin stelly-mix-qobuz-app
```

is stelly-mix's app with both sources, for its Index box.

The gateway is at `STELLY_QOBUZ_URL` (default `http://127.0.0.1:7710`),
with `STELLY_QOBUZ_TOKEN` if it wants one, from the environment or
`~/.config/stelly-mix/qobuz.env`.
