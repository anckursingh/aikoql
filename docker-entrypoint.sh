#!/bin/sh
# The image's entrypoint: `aikoql "$@"`, after one upgrade step. Images
# 0.1.18–0.2.3 kept the aikoql-v2 directory at /data/aikoql.redb (a
# redb-era name); the default is now /data/aikoql. Serving that default on a
# volume that only has the old directory moves it once (a rename within the
# volume), so pull + recreate keeps the knowledge base.
set -e
if [ "$1" = serve ] && [ "$2" = /data/aikoql ] &&
   [ -d /data/aikoql.redb ] && [ ! -e /data/aikoql ]; then
  mv /data/aikoql.redb /data/aikoql
  echo "aikoql: moved the database /data/aikoql.redb to /data/aikoql (its new name)" >&2
fi
exec aikoql "$@"
