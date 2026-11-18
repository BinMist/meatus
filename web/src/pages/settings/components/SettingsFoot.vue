<template>
  <div class="mt-settings-foot">{{ text }}</div>
</template>

<script setup lang="ts">
defineOptions({ name: 'SettingsFoot' })
defineProps<{ text: string }>()
</script>
