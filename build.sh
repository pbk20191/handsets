#!/usr/bin/env bash
# Builds build/mobax.jar. The real pipeline lives in build.gradle.kts.
set -euo pipefail
cd "$(dirname "$0")"
exec ./gradlew --quiet mobaxJar "$@"
