#! /usr/bin/env bash
set -euo pipefail

git revert HEAD
echo "✅ 撤回新版本提交"

LATEST=$(git for-each-ref --sort=-creatordate --format='%(refname:short)' refs/tags | head -1)
git tag -d $LATEST
echo "✅ 删除本地标签 $LATEST"

git push origin --delete $LATEST
echo "✅ 删除远程标签 $LATEST"
