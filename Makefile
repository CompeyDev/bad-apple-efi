# PREREQUISITES
#~~~~~~~~~~~~~~

# Emulation: pacman -S qemu-base qemu-ui-gtk ovmf 
# Python: pacman -S python-pillow python-opencv

.PHONY: default
default: build

.PHONY: build
build:
	cargo build --release --target x86_64-unknown-uefi

.PHONY: qemu-run
qemu-run: build
	mkdir -p .qemu/efi/boot
	cp target/x86_64-unknown-uefi/release/bad-apple.efi .qemu/efi/boot/bootx64.efi
	qemu-system-x86_64 -nodefaults -bios /usr/share/ovmf/x64/OVMF.4m.fd \
		-vga std \
		-machine q35,accel=kvm:tcg \
		-m 512M \
		-drive format=raw,file=fat:rw:.qemu \
		-serial stdio \
		-display gtk \
		-monitor vc:256x192

.PHONY: clean
clean:
	rm -rf bin/bad_apple.mp4 .qemu
	cargo clean
