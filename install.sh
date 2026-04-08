#!/usr/bin/env bash
# Installeur Gemini Lite — télécharge la dernière release GitHub (méthode .deb sur Debian/Ubuntu, sinon binaire).
# Usage : curl -fsSL https://raw.githubusercontent.com/Tormknd/gemini-lite/main/install.sh | bash
# Variables optionnelles :
#   GEMINI_LITE_REPO       — dépôt « owner/name » (défaut : Tormknd/gemini-lite)
#   GEMINI_LITE_INSTALL_DIR — répertoire du binaire (défaut : $HOME/.local/bin)
#   GEMINI_LITE_METHOD     — auto | deb | binary

set -euo pipefail

REPO="${GEMINI_LITE_REPO:-Tormknd/gemini-lite}"
INSTALL_DIR="${GEMINI_LITE_INSTALL_DIR:-$HOME/.local/bin}"
METHOD="${GEMINI_LITE_METHOD:-auto}"
MIN_FREE_KB="${GEMINI_LITE_MIN_FREE_KB:-51200}"
TMP_DIR="${TMPDIR:-/tmp}"

die() {
  echo "erreur: $*" >&2
  exit 1
}

cleanup() {
  local f
  for f in "${CLEANUP_FILES[@]}"; do
    [[ -n "$f" && -f "$f" ]] && rm -f "$f"
  done
}
CLEANUP_FILES=()
trap cleanup EXIT

command -v curl >/dev/null 2>&1 || die "curl est requis (installez-le puis relancez)."

[[ -d "$TMP_DIR" && -w "$TMP_DIR" ]] || die "répertoire temporaire inaccessible en écriture : $TMP_DIR"

free_kb="$(df -Pk "$TMP_DIR" 2>/dev/null | awk 'NR==2 {print $4}')"
[[ -n "${free_kb:-}" && "$free_kb" -ge "$MIN_FREE_KB" ]] \
  || die "espace disque insuffisant sous $TMP_DIR (libre : ${free_kb:-?} KiB, minimum : ${MIN_FREE_KB} KiB)."

fetch_release_json() {
  curl -fsSL \
    -H "Accept: application/vnd.github+json" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    -H "User-Agent: gemini-lite-install-script" \
    "https://api.github.com/repos/${REPO}/releases/latest"
}

RELEASE_JSON="$(fetch_release_json)" || die "impossible de récupérer la dernière release pour ${REPO}."

parse_assets() {
  printf '%s' "$RELEASE_JSON" | python3 -c '
import json, sys
data = json.load(sys.stdin)
tag = data.get("tag_name") or ""
assets = data.get("assets") or []
deb = next((a["browser_download_url"] for a in assets if a.get("name", "").endswith(".deb")), None)
raw = next((a["browser_download_url"] for a in assets if a.get("name") == "gemini-lite-linux-amd64"), None)
sums = next((a["browser_download_url"] for a in assets if a.get("name") == "SHA256SUMS"), None)
print(tag or "")
print(deb or "")
print(raw or "")
print(sums or "")
'
}

mapfile -t PARSED < <(parse_assets)
TAG_NAME="${PARSED[0]}"
DEB_URL="${PARSED[1]}"
BINARY_URL="${PARSED[2]}"
SUMS_URL="${PARSED[3]}"

[[ -n "$BINARY_URL" ]] || die "asset « gemini-lite-linux-amd64 » introuvable dans la release ${TAG_NAME:-?}."

is_debian_like() {
  [[ -f /etc/debian_version ]] && command -v dpkg >/dev/null 2>&1
}

use_deb=false
if [[ "$METHOD" == "deb" ]]; then
  [[ -n "$DEB_URL" ]] || die "paquet .deb non publié pour cette release."
  is_debian_like || die "méthode « deb » demandée mais système non-Debian (utilisez GEMINI_LITE_METHOD=binary)."
  use_deb=true
elif [[ "$METHOD" == "binary" ]]; then
  use_deb=false
elif [[ "$METHOD" == "auto" ]]; then
  if is_debian_like && [[ -n "$DEB_URL" ]]; then
    use_deb=true
  fi
else
  die "GEMINI_LITE_METHOD invalide : $METHOD (attendu : auto, deb ou binary)."
fi

verify_sha256_optional() {
  local file_path="$1"
  local base sums_file line
  base="$(basename "$file_path")"
  [[ -n "${SUMS_URL:-}" ]] || return 0
  command -v sha256sum >/dev/null 2>&1 || return 0
  sums_file="$(mktemp "${TMP_DIR%/}/gemini-lite-sha.XXXXXX")"
  CLEANUP_FILES+=("$sums_file")
  curl -fsSL -H "User-Agent: gemini-lite-install-script" -o "$sums_file" "$SUMS_URL" 2>/dev/null || return 0
  line="$(awk -v b="$base" '$NF == b {print; exit}' "$sums_file")" || return 0
  [[ -n "$line" ]] || return 0
  (cd "$(dirname "$file_path")" && printf '%s\n' "$line" | sha256sum -c -) \
    || die "échec de la vérification SHA256 pour ${base}."
}

if [[ "$use_deb" == true ]]; then
  deb_tmp="${TMP_DIR%/}/gemini-lite-${TAG_NAME}.deb"
  CLEANUP_FILES+=("$deb_tmp")
  echo "→ Installation du paquet Debian (${TAG_NAME})…"
  curl -fsSL -H "User-Agent: gemini-lite-install-script" -o "$deb_tmp" "$DEB_URL"
  verify_sha256_optional "$deb_tmp"
  if [[ "$(id -u)" -eq 0 ]]; then
    dpkg -i "$deb_tmp" || (apt-get install -y -f && dpkg -i "$deb_tmp")
  else
    command -v sudo >/dev/null 2>&1 || die "sudo est requis pour installer le .deb en tant que non-root."
    sudo dpkg -i "$deb_tmp" || sudo sh -c 'apt-get install -y -f && dpkg -i "$1"' _ "$deb_tmp"
  fi
  echo "→ gemini-lite installé via apt/dpkg (${TAG_NAME})."
  exit 0
fi

mkdir -p "$INSTALL_DIR" || die "impossible de créer le répertoire d'installation : $INSTALL_DIR"
[[ -w "$INSTALL_DIR" ]] || die "pas d'écriture sur $INSTALL_DIR (vérifiez les permissions ou choisissez un autre GEMINI_LITE_INSTALL_DIR)."

bin_tmp="${TMP_DIR%/}/gemini-lite-linux-amd64-${TAG_NAME}"
CLEANUP_FILES+=("$bin_tmp")
echo "→ Téléchargement du binaire (${TAG_NAME})…"
curl -fsSL -H "User-Agent: gemini-lite-install-script" -o "$bin_tmp" "$BINARY_URL"
verify_sha256_optional "$bin_tmp"
chmod +x "$bin_tmp"
install -m755 "$bin_tmp" "${INSTALL_DIR%/}/gemini-lite"

echo "→ binaire installé : ${INSTALL_DIR%/}/gemini-lite"
case ":$PATH:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo "ajoutez ${INSTALL_DIR} à votre PATH si nécessaire (ex. export PATH=\"\$HOME/.local/bin:\$PATH\")."
    ;;
esac
