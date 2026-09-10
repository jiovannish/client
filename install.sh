#!/bin/sh
# Apache-2.0. Installs this release; rerun the latest release's script to update.
# Keep execution at the end so a truncated download cannot start installation.
install_jio() (
    set -eu
    version=0.2.0
    [ "$#" -eq 0 ] || { echo 'Usage: sh install.sh (optional JIO_INSTALL_DIR)' >&2; exit 1; }

    case "$(uname -s)/$(uname -m)" in
        Darwin/arm64) target=aarch64-apple-darwin ;;
        Darwin/x86_64) target=x86_64-apple-darwin ;;
        Linux/aarch64|Linux/arm64) target=aarch64-unknown-linux-gnu ;;
        Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
        *) echo 'Jio supports macOS and glibc Linux on ARM64 or x86-64.' >&2; exit 1 ;;
    esac
    for command in curl tar awk install; do
        command -v "$command" >/dev/null || { echo "Missing requirement: $command" >&2; exit 1; }
    done
    if command -v sha256sum >/dev/null; then
        checksum=sha256sum
    elif command -v shasum >/dev/null; then
        checksum=shasum
    else
        echo 'Install sha256sum or shasum first.' >&2
        exit 1
    fi

    install_dir=${JIO_INSTALL_DIR:-${HOME:?HOME or JIO_INSTALL_DIR is required}/.local/bin}
    use_sudo=0
    if [ -z "${JIO_INSTALL_DIR:-}" ]; then
        case ":${PATH:-}:" in
            *":$install_dir:"*) ;;
            *:/usr/local/bin:*)
                if [ -w /usr/local/bin ]; then
                    install_dir=/usr/local/bin
                elif command -v sudo >/dev/null && sudo -n true 2>/dev/null; then
                    install_dir=/usr/local/bin
                    use_sudo=1
                fi
                ;;
        esac
    fi
    install_command() {
        if [ "$use_sudo" -eq 1 ]; then sudo -n "$@"; else "$@"; fi
    }
    case "$install_dir" in
        /*) ;;
        *) echo 'JIO_INSTALL_DIR must be an absolute path.' >&2; exit 1 ;;
    esac
    if [ -L "$install_dir/jio" ] || { [ -e "$install_dir/jio" ] && [ ! -f "$install_dir/jio" ]; }; then
        echo "Refusing to replace a symlink or non-file: $install_dir/jio" >&2
        exit 1
    fi
    install_command mkdir -p "$install_dir"
    temporary=$(mktemp -d "${TMPDIR:-/tmp}/jio-install.XXXXXXXX")
    staged=
    trap '[ -z "$staged" ] || install_command rm -f "$staged"; rm -f "$temporary/archive" "$temporary/checksums" "$temporary/jio" "$temporary/LICENSE" "$temporary/THIRDPARTY.json"; rmdir "$temporary"' EXIT
    trap 'exit 1' HUP INT TERM

    asset=jio-$target.tar.gz
    release=https://github.com/jiovannish/client/releases/download/v$version
    echo "Downloading Jio $version ($target)..."
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 15 --max-time 180 "$release/$asset" --output "$temporary/archive"
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 15 --max-time 30 "$release/SHA256SUMS" --output "$temporary/checksums"
    expected=$(awk -v name="$asset" '$2 == name { print $1; count++ } END { if (count != 1) exit 1 }' "$temporary/checksums")
    case "$expected" in
        ''|*[!0-9a-f]*) echo 'Invalid release checksum.' >&2; exit 1 ;;
    esac
    [ "${#expected}" -eq 64 ] || { echo 'Invalid release checksum length.' >&2; exit 1; }
    if [ "$checksum" = sha256sum ]; then
        actual=$(sha256sum "$temporary/archive" | awk '{print $1}')
    else
        actual=$(shasum -a 256 "$temporary/archive" | awk '{print $1}')
    fi
    [ "$actual" = "$expected" ] || { echo 'Checksum mismatch; existing Jio was not changed.' >&2; exit 1; }

    # Extract only named file contents, never archive paths, permissions or links.
    for member in jio LICENSE THIRDPARTY.json; do
        tar -xzOf "$temporary/archive" "$member" > "$temporary/$member"
        [ -s "$temporary/$member" ] || { echo "Empty release file: $member" >&2; exit 1; }
    done
    chmod 755 "$temporary/jio"
    [ "$("$temporary/jio" --version)" = "jio $version" ] || {
        echo 'Downloaded Jio cannot run or reports an unexpected version.' >&2
        exit 1
    }
    license_dir=$install_dir/../share/jio
    install_command mkdir -p "$license_dir"
    install_command install -m 644 "$temporary/LICENSE" "$temporary/THIRDPARTY.json" "$license_dir/"
    # Stage on the destination filesystem so replacement stays atomic.
    staged=$(install_command mktemp "$install_dir/.jio.XXXXXXXX")
    install_command install -m 755 "$temporary/jio" "$staged"
    install_command mv -f "$staged" "$install_dir/jio"
    staged=
    echo "Installed Jio $version at $install_dir/jio"
    case ":${PATH:-}:" in
        *":$install_dir:"*) ;;
        *)
            echo 'To run jio in this shell, copy and run:'
            # POSIX shell quoting, including custom paths containing apostrophes.
            quoted_dir=$(printf '%s' "$install_dir" | sed "s/'/'\\\\''/g")
            printf "  export PATH='%s':\$PATH\n" "$quoted_dir"
            echo 'Add the same line to your shell configuration for future sessions.'
            ;;
    esac
    echo 'OpenSSH (ssh and ssh-keygen) is required to connect to VMs.'
    echo 'Your API key, VM keys and saved configuration were not changed.'
)

install_jio "$@"
