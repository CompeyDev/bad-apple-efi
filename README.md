# bad-apple-efi

The [bad apple video](https://www.youtube.com/watch?v=FtutLA63Cp8&pp=ygUJYmFkIGFwcGxl) but bootable, powered by 🦀

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
sudo pacman -S qemu-base qemu-ui-gtk ovmf python3 python-pillow python-opencv

# Clone the repository
git clone https://git.devcomp.xyz/DevComp/bad-apple-efi.git

# Generate ASCII frames & compile EFI
make build

# Run in QEMU
make qemu-run
```

### Precompiled
Soon.
