<template>
  <div class="mt-inline-actions">
    <sl-button
      type="ghost"
      size="xs"
      icon-only
      aria-label="复制入口地址"
      title="复制入口地址"
      @click="$emit('copy', row)"
    >
      <sl-icon name="copy" :size="14" />
    </sl-button>
    <sl-button
      type="ghost"
      size="xs"
      icon-only
      aria-label="编辑 proxy"
      title="编辑"
      @click="$emit('edit', row)"
    >
      <sl-icon name="edit" :size="14" />
    </sl-button>
    <sl-popconfirm
      title="删除这条 proxy？"
      :description="`将从客户端移除 ${row.name}，并释放 server 侧端口或域名路由。`"
      confirm-text="删除"
      :loading="deleting === row.name"
      type="danger"
      @confirm="$emit('remove', row)"
    >
      <template #trigger="{ triggerProps }">
        <sl-button
          type="ghost"
          size="xs"
          icon-only
          class="mt-danger-action"
          aria-label="删除 proxy"
          title="删除"
          v-bind="triggerProps"
        >
          <sl-icon name="trash" :size="14" />
        </sl-button>
      </template>
    </sl-popconfirm>
  </div>
</template>

<script setup lang="ts">
import { SlButton } from '@sailonix/ui/button'
import { SlIcon } from '@sailonix/icon'
import { SlPopconfirm } from '@sailonix/ui/popconfirm'
import type { ProxyRow } from '../../types'

defineProps<{ row: ProxyRow; deleting?: string }>()
defineEmits<{
  copy: [row: ProxyRow]
  edit: [row: ProxyRow]
  remove: [row: ProxyRow]
}>()
</script>

<style scoped>
.mt-inline-actions {
  display: flex;
  align-items: center;
  justify-content: flex-start;
  gap: 6px;
  width: 100%;
  min-width: 0;
}

.mt-inline-actions > * {
  flex: 0 0 auto;
}

/* 操作列只保留图标本身，去掉 ghost 按钮的底色、边框和阴影。 */
.mt-inline-actions :deep(.sl-button) {
  border-color: transparent;
  background: transparent;
  box-shadow: none;
}

.mt-inline-actions :deep(.sl-button:hover:not(.sl-button--disabled)),
.mt-inline-actions :deep(.sl-button:active:not(.sl-button--disabled)) {
  border-color: transparent;
  background: transparent;
  box-shadow: none;
}
</style>
