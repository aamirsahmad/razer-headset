# razer-headset

Command-line control for the Razer BlackShark V2 HyperSpeed (2.4 GHz dongle
`1532:0565`, wired `1532:056E`) on Linux.

```
razer-headset play-pause      # toggle the active media player
razer-headset play | pause | next | prev
razer-headset players         # list MPRIS players and their status
razer-headset battery         # e.g. "Battery: 85% (charging)"
razer-headset power-off       # read auto power-off delay
razer-headset power-off 30    # set it to 30 minutes (0 = never, max 255)
razer-headset eq              # show active EQ preset
razer-headset eq game         # flat | game | music | movie | custom | off
razer-headset eq-bands        # show the stored custom curve
razer-headset eq-bands 3 2 1 0 0 0 0 1 2 3   # 31 Hz..16 kHz, -9..+6 dB; also selects custom
razer-headset sidetone 8      # mic monitoring level 0-15, or `off`
razer-headset mic             # position of the mic mute switch
razer-headset dnd on          # block Bluetooth calls while on the dongle
razer-headset led battery     # dongle LED: off | link | battery | warning
razer-headset enhancement     # bass/spatial expander flag (read-only on current firmware)
razer-headset status          # everything above plus firmware and serial
razer-headset setup-udev      # install udev rules for non-root access (--print to inspect)
```

Every command without an argument reads the current value; with an argument
it writes and then reads back. All of them except `enhancement` were verified
to round-trip on dongle/headset firmware 1.6.11.0. The enhancement register
acknowledges writes but keeps its value on that firmware, so the tool reports
that as an error rather than pretending it worked.

## Setup

### 1. Install

Requires Rust (1.100.0+), a C compiler, and `libudev` development headers
(`libudev-dev` + `pkg-config` on Debian/Ubuntu, `systemd-devel` on Fedora,
`systemd-libs` on Arch).

```sh
cargo install razer-headset
```

Or build from source:

```sh
cargo build --release
./target/release/razer-headset status
```

### 2. udev rules (non-root access)

The dongle's hidraw node is root-only by default. On your first headset
command, if `/etc/udev/rules.d/70-razer-blackshark.rules` is not installed yet,
`razer-headset` will detect the permission error and offer to install the rules
and reload udev via `sudo` automatically.

You can also run setup explicitly at any time (or pass `--print` to inspect the
rules first):

```sh
razer-headset setup-udev
```

## How it works

* **Media keys.** The headset's Play/Pause button is an HID *input* on the
  Consumer Control page: it tells the host what to do. Nothing can make the
  headset itself play or pause, so the `play`/`pause`/`next`/`prev` commands do
  what the button does and drive the active media player over MPRIS (D-Bus).
  A playing player is preferred; otherwise the first one found.
* **Battery, power-off, firmware.** The dongle is a MediaTek/Airoha design and
  does **not** speak the classic 90-byte Razer protocol used by OpenRazer.
  Control goes over 64-byte report-ID-`0x02` frames on HID usage page
  `0xFF14`; see `src/protocol.rs` for the layout and register map. Protocol
  knowledge comes from
  [justik13/razer-blackshark-v2-hyperspeed-webhid](https://github.com/justik13/razer-blackshark-v2-hyperspeed-webhid)
  and [bbesli/Synapse-for-Linux](https://github.com/bbesli/Synapse-for-Linux).

Requests addressed to the headset are relayed over the RF link and go
unanswered when it is off, so the tool checks the link status first.

## TODO

* macOS support

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE),
at your option.
