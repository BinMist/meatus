<template>
  <sl-status-tag :status="status" :label="label" size="sm" />
</template>

<script setup lang="ts">
import { SlStatusTag } from '@sailonix/ui/status-tag'

defineOptions({ name: 'CertificateStatusTag' })

defineProps<{
  status: 'default' | 'success' | 'processing' | 'warning' | 'error'
  label: string
}>()
</script>
