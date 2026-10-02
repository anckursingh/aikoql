#!/usr/bin/env bash
# §14: run every Jazzer target under the real engine — the D-19 nightly
# "long fuzz" arm (CI sets AIKOQL_JAZZER_SECONDS for a bigger budget; the
# default is a short smoke). The jazzer-maven-plugin is not published to
# Maven Central, so the standalone driver is the supported path: the
# jazzer + jazzer-api jars arrive via dependency:get, and the driver's
# --target_class discovers the fuzzerTestOneInput(byte[]) targets.
set -euo pipefail
cd "$(dirname "$0")"

mvn -q test-compile
mvn -q dependency:get -Dartifact=com.code-intelligence:jazzer:0.24.0
mvn -q dependency:get -Dartifact=com.code-intelligence:jazzer-api:0.24.0

if [[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* ]]; then
    SEP=';'
    # Windows java cannot read git-bash's POSIX /c/... paths (mixed mode),
    # and MSYS must not rewrite the ;-joined -cp argument at all.
    export MSYS2_ARG_CONV_EXCL='*'
    JAZZER_JAR="$(cygpath -m "$HOME/.m2/repository/com/code-intelligence/jazzer/0.24.0/jazzer-0.24.0.jar")"
    API_JAR="$(cygpath -m "$HOME/.m2/repository/com/code-intelligence/jazzer-api/0.24.0/jazzer-api-0.24.0.jar")"
else
    SEP=':'
    JAZZER_JAR="$HOME/.m2/repository/com/code-intelligence/jazzer/0.24.0/jazzer-0.24.0.jar"
    API_JAR="$HOME/.m2/repository/com/code-intelligence/jazzer-api/0.24.0/jazzer-api-0.24.0.jar"
fi
CP="target/test-classes${SEP}target/classes${SEP}${JAZZER_JAR}${SEP}${API_JAR}"

SECONDS_PER="${AIKOQL_JAZZER_SECONDS:-5}"
for target in FuzzParseRPCResponse FuzzParseMcpError FuzzDecodeToolEnvelope \
        FuzzDecodeStreamNotify FuzzVersionParser FuzzRequestIDCorrelation \
        FuzzErrorMapping; do
    java -cp "$CP" com.code_intelligence.jazzer.Jazzer \
        --target_class="io.aikoql.client.$target" -max_total_time="$SECONDS_PER"
done
