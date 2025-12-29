# bad-apple-efi

The [bad apple video](https://youtu.be/FtutLA63Cp8) but bootable, powered by 🦀

> [!NOTE]  
> The EFI application does not actually include the audio, it has merely been `ffmpeg`'d into the demo video.
> Audio support would involve a complete refactor and removal of all reliance on [GOP](https://wiki.osdev.org/GOP),
> and instead move towards writing directly to the frame buffer, so that [PC Speaker](https://wiki.osdev.org/PC_Speaker) can be used,
> which requires an exit of boot services. [Pull requests](https://github.com/CompeyDev/bad-apple-efi/pulls) are welcome!

<details>
  <summary>Preview</summary>

  https://github.com/CompeyDev/bad-apple-efi/assets/74418041/efc399e5-9ccb-45f0-91a2-301c9ec8657c
</details>


## Usage

### Source
The following guide assumes you have [rustup](https://rustup.rs) and the `base-devel` installed.
The package names are specific to Arch Linux, but install commands can be modified to equivalents for
other distributions.

```sh
# Install prerequisites
sudo pacman -S qemu-base qemu-ui-gtk ovmf

# Clone the repository
git clone https://github.com/CompeyDev/bad-apple-efi.git

# Generate ASCII frames & compile EFI
make build

# Run in QEMU
make qemu-run
```

### Precompiled
Soon.
