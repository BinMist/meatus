<template>
  <sl-panel title="保存行为" variant="filled">
    <div class="mt-note-list">
      <div>
        <span class="mt-note-index">01</span>
        <span class="mt-note-text">保存前校验完整配置；端口变化先做 bind 预检，避免重启后监听失败。</span>
      </div>
      <div>
        <span class="mt-note-index">02</span>
        <span class="mt-note-text">原文件备份为 <code>server.toml.bak</code>；写入使用同目录临时文件并原子替换。</span>
      </div>
      <div>
        <span class="mt-note-index">03</span>
        <span class="mt-note-text">保存成功后进程退出，Docker <code>unless-stopped</code> / systemd <code>Restart=always</code> 会拉起。</span>
      </div>
      <div>
        <span class="mt-note-index">04</span>
        <span class="mt-note-text">认证 token、TLS 证书路径、admin 绑定与会话参数不在线修改。</span>
      </div>
    </div>
  </sl-panel>
</template>

<script setup lang="ts">
import { SlPanel } from '@sailonix/ui/panel'

defineOptions({ name: 'SettingsBehaviorNotes' })
</script>
