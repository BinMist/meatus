# Meatus Skills

本目录存放随仓库分发的 Agent Skill。Skill 是给 AI Agent 读取的目录，不是普通文档；使用时把整个 Skill 目录复制到目标 Agent 的 `.user_skills/` 下，保留 `SKILL.md` 与 `references/` 结构。

## meatus-tutorial

- 定位：第一次使用 Meatus 的新手教程，按“本机体验 / 公网服务器 / Docker”三条路线分步教学。
- 入口：`meatus-tutorial/SKILL.md`
- 课程脚本：`meatus-tutorial/references/lessons.md`
- 检查点与排障：`meatus-tutorial/references/checklist.md`
- 触发示例：“教我使用 Meatus”“从零开始部署”“第一次用”“怎么把本机服务暴露到公网”。

安装示例：

```bash
cp -R docs/skill/meatus-tutorial <agent-workspace>/.user_skills/
```

如果 Agent 的技能目录不是 `.user_skills`，复制到该 Agent 实际加载的 Skill 根目录即可。
