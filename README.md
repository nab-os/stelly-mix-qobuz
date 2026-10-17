# stelly-mix-qobuz

[stelly-mix](https://github.com/nab-os/stelly-mix) with Qobuz as a source.
Private, and separate, so that stelly-mix carries no Qobuz client,
credentials or terms: this crate registers a `Source` and builds its own
binary, which is stelly-mix with `qobuz:` references wherever it takes a
track.

```sh
export STELLY_MIX_QOBUZ_ENV=~/Documents/repos/stelly/.env   # Stelly's login writes it
cargo run --release -- search qobuz "remon verhoeve"
cargo run --release -- index qobuz:favourites
cargo run --release -- set --like "chill downtempo" --out chill.wav
```

References: `qobuz:<track id>`, `qobuz:favourites`, `qobuz:playlist/<id>`,
`qobuz:album/<id>`. Tracks are fetched once, FLAC at CD quality
(`QOBUZ_FORMAT=mp3` for MP3 320), into `~/.cache/stelly-mix/qobuz`, and
analysed and mixed from there. Requests share one 2/s budget, retried on 429
and server faults, as Stelly does.

Credentials: `QOBUZ_APP_ID`, `QOBUZ_APP_SECRETS`, `QOBUZ_USER_AUTH_TOKEN`,
from `$STELLY_MIX_QOBUZ_ENV`, `~/.config/stelly-mix/qobuz.env`, or the
environment.

The Qobuz client is a blocking port of Stelly's `server/src/qobuz.rs`: when
Qobuz changes something, both want the fix.
