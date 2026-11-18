<template>
  <sl-alert
    id="mtLoginError"
    class="mt-login-error"
    type="error"
    variant="subtle"
    :description="text"
    show-icon
    aria-live="polite"
  />
</template>

<script setup lang="ts">
import { SlAlert } from '@sailonix/ui/alert'

defineOptions({ name: 'LoginErrorAlert' })
defineProps<{ text: string }>()
</script>
