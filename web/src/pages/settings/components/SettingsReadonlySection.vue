<template>
  <sl-panel
    id="settings-readonly"
    class="settings-group"
    title="只读运行信息"
    description="为避免误改认证与会话边界，这些字段只能编辑 TOML"
    variant="filled"
  >
    <sl-descriptions
      class="mt-readonly-facts"
      :items="items"
      :column="1"
      layout="horizontal"
      bordered
      size="sm"
    />
  </sl-panel>
</template>

<script setup lang="ts">
import { SlDescriptions } from '@sailonix/ui/descriptions'
import { SlPanel } from '@sailonix/ui/panel'

defineOptions({ name: 'SettingsReadonlySection' })
defineProps<{ items: Array<{ label: string; value: string | number }> }>()
</script>
