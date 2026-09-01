#!/usr/bin/env bash
# Copy the NAS's General catalog into the local library.
#
# Local is the curated side and wins every collision — it has the proper
# `NN Title.ext` names, cover art, and the cross-platform character
# substitutions. The NAS copies of these nine albums are the raw zip-import
# versions (`Artist - Album - NN Title.mp3`) of records already imported
# properly; copying them would put both namings in one folder, which is the
# mistake that duplicated UMFANG's "Ok" during the Dance copy.
set -euo pipefail
SRC="/Volumes/Media/Music/General"
DST="$HOME/MusicLibrary/General"

ALREADY_HAVE=(
  "Anderson .Paak/Malibu"
  "Early James/Strange Time To Be Alive"
  "Imogen Heap/Speak For Yourself (Standard Version)"
  "Jeremie Albino/Tears You Hide"
  "Lewis Del Mar/AUGUST"
  "Lewis Del Mar/BOUQUET"
  "Scott H. Biram/Nothin' But Blood"
  "Sierra Ferrell/Long Time Coming"
  "Sierra Ferrell/Trail Of Flowers"
)

args=(-rt --exclude='.DS_Store' --exclude='*.air' --exclude='*.vdjedit')
for album in "${ALREADY_HAVE[@]}"; do args+=(--exclude="$album"); done
# Both singles are already here, and the local copy carries the fullwidth
# quotes the naming rule requires where the NAS still has hyphens.
args+=(--exclude='Singles')
# These two folders substitute a character; they go in under the real name.
args+=(--exclude='AC_DC' --exclude='Lee _Scratch_ Perry')

rsync "${args[@]}" "$SRC/" "$DST/"
rsync -rt --exclude='.DS_Store' "$SRC/AC_DC/"             "$DST/AC⧸DC/"
rsync -rt --exclude='.DS_Store' "$SRC/Lee _Scratch_ Perry/" "$DST/Lee ＂Scratch＂ Perry/"
echo "COPY COMPLETE"
