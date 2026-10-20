#! /usr/bin/env bash

set -euo pipefail

# 发布新版本时只能是
is_whitelisted() {
    local f="$1"
    [[ "$f" == "Cargo.toml" || "$f" == "Cargo.lock" || "$f" == "agent/Cargo.toml" || "$f" == "agent/Cargo.lock" || "$f" == ".github/release/stable/oxide_changelog.md" || "$f" == ".github/release/stable/release_log" || "$f" == ".github/release/stable/oxide_agent_changelog.md" ]]
}


check_white_list(){
    local bad=() f
    while IFS= read -r -d '' f; do
        [ -z "$f" ] && continue
        is_whitelisted "$f" || bad+=("$f")
    done < <(git diff --name-only -z HEAD)

    while IFS= read -r -d '' f; do
        [ -z "$f" ] && continue
        bad+=("$f")
    done < <(git ls-files --others --exclude-standard -z)

    if [ ${#bad[@]} -gt 0 ]; then
        echo "❌ 拒绝发布，存在未提交变更" >&2
        printf ' %s\n' "${bad[@]}" >&2
        return 1
    fi
}

# 确保发版时，全部工作已提交
check_white_list

[ "$(git branch --show-current)" == "main" ] || {
  echo "❌ not on main branch" >&2; exit 1;
}
echo "✅ 分支校验成功: 当前处于main分支"
# 读取最后一行发布日志，最后一行表示最新的发布版本
last_line=$(tail -n 1 .github/release/stable/release_log)
# 以“-”为分隔符，获取软件名称、发布渠道、版本号和发布标签
IFS='_' read -r NAME CHANNEL VERSION TAG <<< "$last_line"

if [[ -z "${NAME:-}" || -z "${CHANNEL:-}" || -z "${VERSION:-}" || -z "${TAG:-}" ]]; then
    echo "❌ release_log 格式错误: $last_line"
    exit 1
fi

# 校验版本号格式
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-].+)?$ ]] || {
  echo "❌ 版本号错误: $0 <x.y.z>" >&2; exit 1;
}

echo "✅ 发布日志读取成功: 发布${NAME}-${CHANNEL}-${TAG}"

GIT_TAG="${NAME}-v${VERSION}"
if git rev-parse "$GIT_TAG" >/dev/null 2>&1; then
  echo "❌ Git tag $GIT_TAG 已存在" >&2; exit 1;
fi

git add ".github/release/stable/release_log" ".github/release/stable/${NAME//-/_}_changelog.md"

if [ "$NAME" == "oxide" ]; then
    git add Cargo.toml Cargo.lock
elif [ "$NAME" == "oxide-agent" ]; then
    git add "agent/Cargo.toml" "agent/Cargo.lock"
else
    echo "❌ release_log 格式错误" >&2; exit 1;
fi

if git diff --cached --quiet; then
  echo "❌ 发布新版本未变更版本号以及未撰写发布日志" >&2; exit 1;
fi

git commit -m "release: release ${NAME} v${VERSION}"

echo "✅ 发布日志提交成功"

HASH="$(git rev-parse HEAD)"

git tag -a "$GIT_TAG" "$HASH" -m "release ${NAME} v${VERSION}"
echo "✅ 本地标签创建成功"

git push origin "refs/tags/$GIT_TAG"
echo "✅ 标签推送成功"

git push origin main
echo "✅ 新版本 ${GIT_TAG} @ ${HASH} 版本推送成功"