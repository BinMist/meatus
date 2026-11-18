<template>
  <aside class="settings-nav">
    <div class="settings-nav-title">配置分区</div>
    <sl-anchor
      class="settings-anchor"
      :items="items"
      :active-key="activeKey"
      target=".sl-app-shell__content"
      :offset-top="0"
      aria-label="配置分区"
      @click="handleSelect"
    />
    <div class="settings-nav-note">
      认证、会话与 TLS 路径只读；保存会备份原文件并重启 server。
    </div>
  </aside>
</template>

<script setup lang="ts">
import { SlAnchor } from '@sailonix/ui/anchor'
import type { AnchorItem } from '@sailonix/ui/anchor'
import type { SettingsNavItem } from '../types'

defineOptions({ name: 'SettingsNav' })

defineProps<{ items: SettingsNavItem[]; activeKey: string }>()
const emit = defineEmits<{ select: [key: string] }>()

function handleSelect(item: AnchorItem) {
  emit('select', item.key)
}
</script>
