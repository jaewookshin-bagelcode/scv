#!/bin/sh
# scv 심볼릭 링크 설치/제거.
#
# `scv` 를 PATH 의 bin 디렉터리에 심볼릭 링크로 건다. 링크는 release 바이너리
# (target/release/scv)를 가리키므로, 코드 수정 후 `cargo build --release` 만 다시 하면
# 재설치 없이 링크가 최신 바이너리를 가리킨다.
#
# install 은 `skills/` 의 **기본 스킬**도 전역 스킬 디렉터리(~/.scv/skills)로 복사한다(기존 것은 보존).
#
# 사용:
#   sh scripts/scv-link.sh install        # release 빌드 + 링크 생성 + 기본 스킬 설치(기본)
#   sh scripts/scv-link.sh uninstall      # 링크 제거(전역 스킬은 사용자 데이터라 남긴다)
#   sh scripts/scv-link.sh purge [-y]     # 링크 + 전역 스킬(~/.scv/skills) + release 바이너리까지 완전 제거
#   sh scripts/scv-link.sh status         # 링크/PATH 상태
#
# bin 디렉터리 선택(우선순위): $SCV_BIN_DIR → PATH 에 있는 ~/.local/bin → PATH 에 있는
#   ~/.cargo/bin → ~/.local/bin(없으면 PATH 추가 안내). 직접 지정: SCV_BIN_DIR=/path ...
# 스킬 설치 위치: $SCV_SKILLS_DIR (기본 ~/.scv/skills).

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd -P)
TARGET="$REPO_ROOT/target/release/scv"
SKILLS_SRC="$REPO_ROOT/skills"
SKILLS_DEST="${SCV_SKILLS_DIR:-$HOME/.scv/skills}"

# PATH 에 이미 있는 bin 디렉터리를 골라 "자동 호출"이 바로 되게 한다.
pick_bin_dir() {
    if [ -n "${SCV_BIN_DIR:-}" ]; then
        printf '%s' "$SCV_BIN_DIR"
        return
    fi
    # 이미 PATH 에서 잡히는 scv 가 후보 dir 에 있으면 그 자리를 덮어 섀도잉을 막는다
    # (예: 과거 `cargo install` 복사본이 ~/.cargo/bin 에 있으면 그걸 링크로 대체).
    existing=$(command -v scv 2>/dev/null || true)
    if [ -n "$existing" ]; then
        existing_dir=$(dirname -- "$existing")
        for d in "$HOME/.local/bin" "$HOME/.cargo/bin"; do
            [ "$existing_dir" = "$d" ] && { printf '%s' "$d"; return; }
        done
    fi
    for d in "$HOME/.local/bin" "$HOME/.cargo/bin"; do
        case ":$PATH:" in
            *":$d:"*) printf '%s' "$d"; return ;;
        esac
    done
    printf '%s' "$HOME/.local/bin"
}

BIN_DIR=$(pick_bin_dir)
LINK="$BIN_DIR/scv"

on_path() {
    case ":$PATH:" in
        *":$BIN_DIR:"*) return 0 ;;
        *) return 1 ;;
    esac
}

# 레포의 기본 스킬(skills/<name>/SKILL.md)을 전역 스킬 디렉터리($SCV_SKILLS_DIR,
# 기본 ~/.scv/skills)로 복사한다. **기존 스킬은 덮어쓰지 않는다**(사용자 편집 보존).
# scv 가 `/<name>` 으로 발동한다.
install_skills() {
    [ -d "$SKILLS_SRC" ] || return 0
    mkdir -p "$SKILLS_DEST"
    for dir in "$SKILLS_SRC"/*/; do
        [ -f "$dir/SKILL.md" ] || continue
        name=$(basename "$dir")
        if [ -e "$SKILLS_DEST/$name" ]; then
            echo "[scv] skill '$name' already present — keeping yours"
        else
            cp -R "$dir" "$SKILLS_DEST/$name"
            echo "[scv] installed skill: $name"
        fi
    done
}

cmd=${1:-install}
case "$cmd" in
    install)
        echo "[scv] building release binary…"
        ( cd "$REPO_ROOT" && cargo build --release --bin scv )
        mkdir -p "$BIN_DIR"
        ln -sf "$TARGET" "$LINK"
        echo "[scv] linked: $LINK -> $TARGET"
        if on_path; then
            echo "[scv] $BIN_DIR is on PATH — run 'scv' from any project directory."
        else
            echo "[scv] WARNING: $BIN_DIR is not on PATH. add this to your shell rc:"
            echo "         export PATH=\"$BIN_DIR:\$PATH\""
        fi
        # 다른 위치의 scv 가 PATH 우선순위로 이 링크를 가리는지 경고.
        resolved=$(command -v scv 2>/dev/null || true)
        if [ -n "$resolved" ] && [ "$resolved" != "$LINK" ]; then
            echo "[scv] WARNING: another 'scv' shadows the link: $resolved"
            echo "         remove it (e.g. 'cargo uninstall scv-cli' or rm) or reorder PATH."
        fi
        # 기본 스킬 설치(기존 보존). TUI 에서 /skills 로 확인, /<name> 으로 발동.
        install_skills
        ;;
    uninstall)
        if [ -L "$LINK" ]; then
            rm -f "$LINK"
            echo "[scv] removed link: $LINK"
        else
            echo "[scv] no symlink at $LINK (nothing to remove)"
        fi
        ;;
    purge)
        # uninstall 과 달리 사용자 데이터(스킬)와 빌드 산출물까지 전부 지운다.
        # 되돌릴 수 없으므로 무엇을 지울지 먼저 보여주고 확인을 받는다(-y/--yes 로 생략).
        assume_yes=false
        case "${2:-}" in
            -y|--yes) assume_yes=true ;;
            "") ;;
            *) echo "usage: sh scripts/scv-link.sh purge [-y|--yes]" >&2; exit 2 ;;
        esac

        # 1) 지울 대상을 수집·표시한다(스킬은 레포 skills/ 에 대응하는 설치본만).
        found=false
        echo "[scv] purge will remove:"
        if [ -L "$LINK" ]; then
            echo "  - link:    $LINK"
            found=true
        fi
        if [ -e "$TARGET" ]; then
            echo "  - binary:  $TARGET"
            found=true
        fi
        if [ -d "$SKILLS_SRC" ] && [ -d "$SKILLS_DEST" ]; then
            for dir in "$SKILLS_SRC"/*/; do
                [ -f "$dir/SKILL.md" ] || continue
                name=$(basename "$dir")
                if [ -e "$SKILLS_DEST/$name" ]; then
                    echo "  - skill:   $SKILLS_DEST/$name"
                    found=true
                fi
            done
        fi
        if [ "$found" != true ]; then
            echo "  (nothing found — already clean)"
            exit 0
        fi

        # 2) 확인(비대화형은 -y/--yes).
        if [ "$assume_yes" != true ]; then
            printf '[scv] proceed? [y/N] '
            read -r ans || ans=""
            case "$ans" in
                y|Y|yes|Yes|YES) ;;
                *) echo "[scv] aborted (nothing removed)."; exit 0 ;;
            esac
        fi

        # 3) 삭제.
        if [ -L "$LINK" ]; then
            rm -f "$LINK"
            echo "[scv] removed link: $LINK"
        fi
        if [ -e "$TARGET" ]; then
            rm -f "$TARGET"
            echo "[scv] removed binary: $TARGET"
        fi
        if [ -d "$SKILLS_SRC" ] && [ -d "$SKILLS_DEST" ]; then
            for dir in "$SKILLS_SRC"/*/; do
                [ -f "$dir/SKILL.md" ] || continue
                name=$(basename "$dir")
                if [ -e "$SKILLS_DEST/$name" ]; then
                    rm -rf "$SKILLS_DEST/$name"
                    echo "[scv] removed skill: $name"
                fi
            done
            # 스킬 디렉터리가 비었으면 같이 정리(다른 스킬이 남아 있으면 보존).
            rmdir "$SKILLS_DEST" 2>/dev/null && echo "[scv] removed empty dir: $SKILLS_DEST" || true
        fi
        ;;
    status)
        if [ -L "$LINK" ]; then
            echo "[scv] link: $LINK -> $(readlink "$LINK")"
        else
            echo "[scv] not linked at $LINK"
        fi
        if command -v scv >/dev/null 2>&1; then
            echo "[scv] 'scv' resolves to: $(command -v scv)"
        else
            echo "[scv] 'scv' is not on PATH"
        fi
        ;;
    *)
        echo "usage: sh scripts/scv-link.sh [install|uninstall|purge|status]" >&2
        echo "  env: SCV_BIN_DIR (default: PATH 의 ~/.local/bin 또는 ~/.cargo/bin)" >&2
        echo "       SCV_SKILLS_DIR (default: ~/.scv/skills)" >&2
        exit 2
        ;;
esac
