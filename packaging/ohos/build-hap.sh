#!/usr/bin/env bash
# Build a simulator HAP without writing generated files into the source tree.
set -euo pipefail

packaging_dir="$(cd "$(dirname "$0")" && pwd)"
repo_dir="$(cd "$packaging_dir/../.." && pwd)"
deveco="${DEVECO_HOME:-/Applications/DevEco-Studio.app/Contents}"
export PATH="$deveco/tools/node/bin:$deveco/tools/ohpm/bin:$PATH"
export JAVA_HOME="$deveco/jbr/Contents/Home"
export DEVECO_SDK_HOME="$deveco/sdk"
export OHOS_NDK_HOME="${OHOS_NDK_HOME:-$deveco/sdk/default/openharmony}"
ability_dir="${OPENHARMONY_ABILITY_DIR:-$repo_dir/../openharmony-ability}"
project_dir="$repo_dir/target/photocraft-hap"

"$packaging_dir/build-rust.sh" "$@"
mkdir -p "$project_dir"
cp -R "$packaging_dir/project/." "$project_dir/"
rm -rf "$project_dir/native_ability" "$project_dir/plugins"
cp -R "$ability_dir/native_ability" "$ability_dir/plugins" "$project_dir/"
rm -f "$project_dir"/plugins/*/oh-package-lock.json5
perl -pi -e 's/"\@ohos-rs\/ability":\s*"workspace:\*"/"\@ohos-rs\/ability": "file:..\/..\/native_ability"/g; s/"\@ohos-rs\/ability-plugin-menu":\s*"workspace:\*"/"\@ohos-rs\/ability-plugin-menu": "file:..\/menu"/g' "$project_dir"/plugins/*/oh-package.json5
(cd "$project_dir" && patch -p1 < "$packaging_dir/files-save.patch")
mkdir -p "$project_dir/entry/src/main/ets/entryability" "$project_dir/entry/src/main/ets/pages" \
    "$project_dir/entry/libs/arm64-v8a"
cp "$packaging_dir/arkts/EntryAbility.ets" "$project_dir/entry/src/main/ets/entryability/"
cp "$packaging_dir/arkts/Index.ets" "$project_dir/entry/src/main/ets/pages/"
cp "$repo_dir/target/aarch64-unknown-linux-ohos/release/libphotocraft_ohos.so" \
    "$project_dir/entry/libs/arm64-v8a/"
for media in "$project_dir/AppScope/resources/base/media" "$project_dir/entry/src/main/resources/base/media"; do
    mkdir -p "$media"
    cp "$repo_dir/assets/app-icon/hicolor/256x256/apps/ai.storyteller.photocraft.png" "$media/app_icon.png"
done
printf 'sdk.dir=%s\n' "$deveco/sdk" > "$project_dir/local.properties"
cd "$project_dir"
ohpm install --log_level warn
"$deveco/tools/hvigor/bin/hvigorw" --mode module -p product=default -p module=entry@default \
    -p buildMode=debug assembleHap --no-daemon
printf '\nHAP: %s/entry/build/default/outputs/default/entry-default-unsigned.hap\n' "$project_dir"
