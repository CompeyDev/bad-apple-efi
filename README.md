# bad-apple-efi

The [bad apple video](https://youtu.be/FtutLA63Cp8) but bootable, powered by 🦀

> [!NOTE]  
> The EFI application does not support audio yet. Audio support has been planned, and will likely be 
> implemented using [PC Speaker](https://wiki.osdev.org/PC_Speaker), and a simple MIDI track. Open a
> [Pull Request](https://github.com/CompeyDev/bad-apple-efi/pulls) if you have a more ambitious method
> in mind!

<details>
  <summary>Preview</summary>

  https://github.com/user-attachments/assets/f4a59731-22d5-4b55-8f0b-b9947a492434
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
