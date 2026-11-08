# bad-apple-efi

The [bad apple video](https://youtu.be/FtutLA63Cp8) but bootable, powered by 🦀

<details>
  <summary>Preview</summary>

  https://github.com/user-attachments/assets/f4a59731-22d5-4b55-8f0b-b9947a492434
</details>


## Requirements
The CPU must support AVX2 and FMA, which means an Intel Haswell (2013), AMD Excavator (2015)
or newer CPU. If you are unsure whether your CPU supports these features, try booting and
an unsupported CPU would display an error.

The system must boot through UEFI, legacy BIOS is not supported.

## Usage

### Source
The following guide assumes you have [rustup](https://rustup.rs) and the `base-devel` installed.
The package names are specific to Arch Linux, but install commands can be modified to equivalents for
other distributions.

```sh
# Install prerequisites
sudo pacman -S qemu-base qemu-ui-gtk ovmf clang llvm lld nasm ffmpeg

# Clone the repository
git clone --recurse-submodules https://github.com/CompeyDev/bad-apple-efi.git
cd bad-apple-efi

# Provide the video (see video section)
cp /path/to/bad_apple.webm bin/

# Build and run in QEMU 
cargo xtask qemu-run
```

#### Controls
| Key                                                            | Action            |
|----------------------------------------------------------------|-------------------|
| <kbd>Space</kbd>, <kbd>Play</kbd>                              | Play or Pause     |
| <kbd>Up</kbd> <kbd>Down</kbd>, <kbd>Vol+</kbd> <kbd>Vol-</kbd> | Change the volume |
| <kbd>M</kbd>, <kbd>Mute</kbd>                                  | Mute or Unmute    |
| <kbd>Esc</kbd>, <kbd>Stop</kbd>                                | Shut down         |

#### Audio
Audio is played through the first HD Audio controller with an analog output, such as speakers
or a headphone jack. Digital outputs like HDMI are not supported.
Only PS/2 keyboards are supported, which includes most built-in laptop keyboards.

#### Video
You must supply the video as an MP4 or WEBM file in the `bin/` directory. Any codec
is accepted, `ffmpeg` is used to convert it to a compatible VP9 stream at build time.

The final VP9 stream is cached across builds, so the encoding process only runs once.

#### Colour
By default, the video is shaded in "two tone" mode for a more retro look. Full color
from the original display can be displayed by enabling the `high_res` build during
the build.

Arguments after an xtask's name are passed to cargo, and for `qemu-run`, arguments
after `--` are passed to QEMU:

```sh
cargo xtask qemu-run --features high_res   # full colour
cargo xtask qemu-run -- -display none      # run headless
```

### Precompiled
Soon.
