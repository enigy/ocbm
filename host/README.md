# host/ — the host end of OCBM, for the bench

The host app is [Carlink](https://github.com/enigy/carlink), on an Android Automotive head unit. This
directory holds only the bench client.

| Dir | What |
|---|---|
| `ocbm-host/` | the host end of the OCBM multiplexer (Rust, rusb/libusb) — claims the accessory interface and speaks OCBM over its bulk endpoints. Subcommands: `hello`, `console` (root shell over `MODE_SELECT CONSOLE`, see [`../docs/ops/01_RECOVERY.md`](../docs/ops/01_RECOVERY.md)), `settime`, `ncm`, `install info\|confirm\|stock`, `push`, `pull`, `av`, `session`, `avdec` (SUBSCRIBE + heartbeat + decrypt the forwarded A/V). `./build.sh` builds it to `target/release/ocbm-host` |

The macOS host app, the in-tree Android apps and the Android Auto tools that lived here left in OCBM
1.0.5; they are at tag [`v1.0.4`](https://github.com/enigy/ocbm/tree/v1.0.4/host).
