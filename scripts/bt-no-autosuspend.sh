#!/usr/bin/env bash
# Disable USB autosuspend on this desktop and recover the Intel AC9260
# Bluetooth adapter (8087:0025) whose firmware load races the suspend timer.
# INC-KOI021 / INC-KOI039. Run as: sudo bash scripts/bt-no-autosuspend.sh
set -u
[ "$(id -u)" -eq 0 ] || { echo "run with sudo"; exit 1; }

dev=""
for d in /sys/bus/usb/devices/*; do
  [ "$(cat "$d/idVendor" 2>/dev/null):$(cat "$d/idProduct" 2>/dev/null)" = "8087:0025" ] && dev=$d && break
done
[ -n "$dev" ] || { echo "adapter 8087:0025 not found on the USB bus"; exit 1; }
echo "adapter: $dev"

# Persist: driver option, hwdb override, kernel command line.
echo 'options btusb enable_autosuspend=0' > /etc/modprobe.d/koi-btusb.conf
printf 'usb:v8087p0025*\n ID_AUTOSUSPEND=0\n' > /etc/udev/hwdb.d/61-koi-bt-no-autosuspend.hwdb
systemd-hwdb update
if ! grep -q 'usbcore.autosuspend=-1' /etc/default/grub; then
  cp /etc/default/grub /etc/default/grub.pre-koi-autosuspend
  sed -i 's/^GRUB_CMDLINE_LINUX_DEFAULT="/&usbcore.autosuspend=-1 /' /etc/default/grub
  update-grub
fi
update-initramfs -u

# Runtime: stop suspend now, then rebind the driver.
echo -1 > /sys/module/usbcore/parameters/autosuspend
echo -1 > "$dev/power/autosuspend_delay_ms"
echo on > "$dev/power/control"
modprobe -r btusb && modprobe btusb
sleep 3
systemctl restart bluetooth
sleep 2

echo "--- result"
grep -o 'usbcore.autosuspend=[^ "]*' /etc/default/grub
echo "power/control: $(cat "$dev/power/control")"
bluetoothctl show | head -3
