<template>
  <div class="mt-domain">
    <span class="mt-domain__name">{{ domain }}</span>
    <span class="mt-domain__meta">{{ meta }}</span>
  </div>
</template>

<script setup lang="ts">
defineOptions({ name: 'CertificateDomain' })

defineProps<{ domain: string; meta: string }>()
</script>
