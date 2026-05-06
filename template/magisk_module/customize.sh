SKIPUNZIP=1

MODULE_ID=@MODULE_ID@

if [ "$ARCH" != "arm" ] && [ "$ARCH" != "arm64" ] && [ "$ARCH" != "x86" ] && [ "$ARCH" != "x64" ]; then
  abort "! Unsupported platform: $ARCH"
else
  ui_print "- Device platform: $ARCH"
fi

ui_print "- Extracting verify.sh"
unzip -o "$ZIPFILE" 'verify.sh' -d "$TMPDIR" >&2
if [ ! -f "$TMPDIR/verify.sh" ]; then
  ui_print    "*********************************************************"
  ui_print    "! Unable to extract verify.sh!"
  ui_print    "! This zip may be corrupted, please try downloading again"
  abort "*********************************************************"
fi
. $TMPDIR/verify.sh

ui_print "- Extracting module files"
extract "$ZIPFILE" 'module.prop' "$MODPATH"

LIB32_NAME="armeabi-v7a.so"
LIB64_NAME="arm64-v8a.so"
LIB32_DEST="$MODPATH/zygisk"
LIB64_DEST="$MODPATH/zygisk"
BUSYBOX_BIN=/data/adb/magisk/busybox

if [ ! -f $BUSYBOX_BIN ]; then
  BUSYBOX_BIN=/data/adb/ksu/bin/busybox
fi

if [ ! -f $BUSYBOX_BIN ]; then
  BUSYBOX_BIN=/data/adb/ap/bin/busybox
fi

if [ ! -f $BUSYBOX_BIN ]; then
  abort "! unable to locate busybox"
fi

ui_print "- Using busybox: $BUSYBOX_BIN"

[ "$ARCH" = "x86" ] || [ "$ARCH" = "x64" ] && LIB32_NAME="x86.so"
[ "$ARCH" = "x86" ] || [ "$ARCH" = "x64" ] && LIB64_NAME="x86_64.so"

mkdir -p "$LIB32_DEST"
mkdir -p "$LIB64_DEST"

ui_print "- Extracting 32-bit libraries"
extract "$ZIPFILE" "lib/$LIB32_NAME" "$LIB32_DEST" true

if [ "$IS64BIT" = true ]; then
  ui_print "- Extracting 64-bit libraries"
  extract "$ZIPFILE" "lib/$LIB64_NAME" "$LIB64_DEST" true
fi

ui_print "- Extracting bundled Frida Gadget"

extract "$ZIPFILE" "gadget/libgadget-$ARCH.so.xz" "$MODPATH" true
mv "$MODPATH/libgadget-$ARCH.so.xz" "$MODPATH/libgadget.so.xz"
rm -f "$MODPATH/libgadget.so"
$BUSYBOX_BIN unxz "$MODPATH/libgadget.so.xz"
rm "$MODPATH/libgadget.so.xz"

if [ "$IS64BIT" = true ]; then
  ARCH32="arm"
  [ "$ARCH" = "x64" ] && ARCH32="x86"

  extract "$ZIPFILE" "gadget/libgadget-$ARCH32.so.xz" "$MODPATH" true
  mv "$MODPATH/libgadget-$ARCH32.so.xz" "$MODPATH/libgadget32.so.xz"
  rm -f "$MODPATH/libgadget32.so"
  $BUSYBOX_BIN unxz "$MODPATH/libgadget32.so.xz"
  rm "$MODPATH/libgadget32.so.xz"
fi

extract "$ZIPFILE" "config.json.example" "$MODPATH" true

set_perm_recursive "$MODPATH" 0 0 0755 0644
