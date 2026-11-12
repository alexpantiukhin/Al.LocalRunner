#!/bin/sh
set -eu

RELEASES_URL="https://github.com/alexpantiukhin/Al.LocalRunner/releases"
RELEASE_KEY="@RELEASE_PUBKEY@"
SIG_NAMESPACE="al-localrunner-release"

die() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}

step() {
    printf 'install.sh: %s
' "$*" >&2
}

usage() {
    cat <<'EOF'
Usage: install.sh [--version X.Y.Z] [--dir DIR] [--offline ARCHIVE]
                  [--role hub|broker|runner|all] [--allow-elevated] [--upgrade]

Installs the localrunner binary for the current user after checking the
signature of SHA256SUMS and the SHA-256 of the archive. --offline takes a
downloaded archive with SHA256SUMS and SHA256SUMS.sig next to it. --upgrade
runs `localrunner upgrade` instead of `install`: it replaces an installed
binary and restarts the roles that run (it asks first; see --help of upgrade).
EOF
}

parse_args() {
    VERSION=""
    DIR=""
    OFFLINE=""
    ROLE="all"
    ALLOW_ELEVATED=0
    UPGRADE=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --version)
                [ $# -ge 2 ] || die "--version needs a value"
                VERSION=$2
                shift 2
                ;;
            --dir)
                [ $# -ge 2 ] || die "--dir needs a value"
                DIR=$2
                shift 2
                ;;
            --offline)
                [ $# -ge 2 ] || die "--offline needs an archive"
                OFFLINE=$2
                shift 2
                ;;
            --role)
                [ $# -ge 2 ] || die "--role needs a value"
                ROLE=$2
                shift 2
                ;;
            --allow-elevated)
                ALLOW_ELEVATED=1
                shift
                ;;
            --upgrade)
                UPGRADE=1
                shift
                ;;
            -h | --help)
                usage
                exit 0
                ;;
            *)
                die "unknown argument: $1 (see --help)"
                ;;
        esac
    done
    case "$ROLE" in
        hub | broker | runner | all) ;;
        *) die "--role must be hub, broker, runner or all" ;;
    esac
    VERSION=${VERSION#v}
    case "$VERSION" in
        *[!0-9A-Za-z.+-]*) die "--version must look like 1.2.3" ;;
    esac
}

check_user() {
    if [ "$(id -u)" = 0 ] && [ "$ALLOW_ELEVATED" != 1 ]; then
        die "refusing to run as root: install as an ordinary user, or add --allow-elevated"
    fi
}

detect_target() {
    case "$(uname -s)" in
        Linux) os=linux ;;
        Darwin) os=macos ;;
        *) die "unsupported OS $(uname -s): use the release archive or build from source" ;;
    esac
    case "$(uname -m)" in
        x86_64 | amd64) arch=x64 ;;
        aarch64 | arm64) arch=arm64 ;;
        *) die "unsupported architecture $(uname -m)" ;;
    esac
    printf '%s-%s\n' "$os" "$arch"
}

fetch() {
    step "downloading $1"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 -o "$2" "$1" || die "download failed: $1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --https-only -O "$2" "$1" || die "download failed: $1"
    else
        die "curl or wget is needed to download the release"
    fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 -r "$1" | cut -d ' ' -f 1
    else
        die "sha256sum, shasum or openssl is needed to check the archive"
    fi
}

verify_signature() {
    step "checking the signature of SHA256SUMS"
    case "$RELEASE_KEY" in
        "" | @*@) die "this install.sh carries no release key: take install.sh from the release page" ;;
    esac
    command -v ssh-keygen >/dev/null 2>&1 ||
        die "ssh-keygen (OpenSSH 8.1 or newer) is needed to check the release signature"
    [ -f "$2" ] || die "signature $2 is missing: nothing installed"
    signers="$WORK/allowed_signers"
    printf '%s namespaces="%s" %s\n' "$SIG_NAMESPACE" "$SIG_NAMESPACE" "$RELEASE_KEY" >"$signers"
    if ! ssh-keygen -Y verify -f "$signers" -I "$SIG_NAMESPACE" -n "$SIG_NAMESPACE" -s "$2" <"$1" >/dev/null 2>"$WORK/verify.err"; then
        if grep -qiE '(illegal|unknown) option' "$WORK/verify.err"; then
            die "this ssh-keygen cannot check signatures: OpenSSH 8.1 or newer is needed"
        fi
        die "the signature of SHA256SUMS does not match the release key: nothing installed"
    fi
}

verify_checksum() {
    step "checking SHA-256 of $3"
    expected=$(awk -v n="$3" '$2 == n || $2 == "*" n { print tolower($1); exit }' "$1")
    [ -n "$expected" ] || die "$3 is not listed in SHA256SUMS: nothing installed"
    actual=$(sha256_of "$2" | tr 'A-F' 'a-f')
    [ "$expected" = "$actual" ] || die "SHA-256 of $3 does not match SHA256SUMS: nothing installed"
}

extract() {
    step "unpacking the archive"
    mkdir "$WORK/bin"
    tar -xzf "$1" -C "$WORK/bin" localrunner || die "cannot unpack $1"
    [ -f "$WORK/bin/localrunner" ] || die "$1 has no localrunner binary"
    chmod 0755 "$WORK/bin/localrunner"
}

run_install() {
    if [ "$UPGRADE" = 1 ]; then
        step "running localrunner upgrade"
        set -- upgrade
    else
        step "running localrunner install"
        set -- install --role "$ROLE"
    fi
    if [ -n "$DIR" ]; then
        set -- "$@" --dir="$DIR"
    fi
    if [ "$ALLOW_ELEVATED" = 1 ]; then
        set -- "$@" --allow-elevated
    fi
    "$WORK/bin/localrunner" "$@" || die "localrunner $1 failed"
}

run_doctor() {
    step "running localrunner doctor"
    if ! "$WORK/bin/localrunner" doctor; then
        printf 'install.sh: warning: localrunner doctor reported problems (above); the installation is kept\n' >&2
    fi
}

main() {
    parse_args "$@"
    check_user
    WORK=$(mktemp -d 2>/dev/null || mktemp -d -t al-localrunner)
    trap 'rm -rf "$WORK"' EXIT
    trap 'exit 1' HUP INT TERM
    if [ -n "$OFFLINE" ]; then
        [ -f "$OFFLINE" ] || die "$OFFLINE is not a file"
        name=$(basename "$OFFLINE")
        from="$(dirname "$OFFLINE")/SHA256SUMS"
        [ -f "$from" ] || die "SHA256SUMS must lie next to $OFFLINE"
        sums="$WORK/SHA256SUMS"
        archive="$WORK/$name"
        cp "$OFFLINE" "$archive" || die "cannot copy $OFFLINE"
        cp "$from" "$sums" || die "cannot copy $from"
        if [ -f "$from.sig" ]; then
            cp "$from.sig" "$sums.sig" || die "cannot copy $from.sig"
        fi
    else
        name="al-localrunner-$(detect_target).tar.gz"
        if [ -n "$VERSION" ]; then
            base="$RELEASES_URL/download/v$VERSION"
        else
            base="$RELEASES_URL/latest/download"
        fi
        sums="$WORK/SHA256SUMS"
        archive="$WORK/$name"
        fetch "$base/SHA256SUMS" "$sums"
        fetch "$base/SHA256SUMS.sig" "$sums.sig"
        fetch "$base/$name" "$archive"
    fi
    verify_signature "$sums" "$sums.sig"
    verify_checksum "$sums" "$archive" "$name"
    extract "$archive"
    run_install
    run_doctor
}

if [ "${LOCALRUNNER_INSTALL_SOURCED:-}" != 1 ]; then
    main "$@"
else
    printf 'install.sh: LOCALRUNNER_INSTALL_SOURCED=1 is set: functions loaded for tests, nothing installed\n' >&2
fi
