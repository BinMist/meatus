<template>
  <div class="mt-expiry">
    <span class="mt-mono-cell">{{ text }}</span>
    <span class="mt-expiry__days" :class="`is-${tone}`">{{ daysText }}</span>
  </div>
</template>

<script setup lang="ts">
defineOptions({ name: 'CertificateExpiry' })

defineProps<{
  text: string
  daysText: string
  tone: 'ok' | 'warn' | 'danger'
}>()
</script>
