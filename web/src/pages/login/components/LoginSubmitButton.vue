<template>
  <sl-button class="mt-login-submit" :class="{ 'is-busy': busy }" type="primary" size="md" native-type="submit" :disabled="busy">
    <span>{{ busy ? '正在验证…' : '验证并进入' }}</span>
    <template #trailing>
      <svg viewBox="0 0 256 256" fill="currentColor" aria-hidden="true"><path d="M221.66,133.66l-72,72a8,8,0,0,1-11.32-11.32L196.69,136H40a8,8,0,0,1,0-16H196.69L138.34,61.66a8,8,0,0,1,11.32-11.32l72,72A8,8,0,0,1,221.66,133.66Z" /></svg>
    </template>
  </sl-button>
</template>

<script setup lang="ts">
import { SlButton } from '@sailonix/ui/button'

defineOptions({ name: 'LoginSubmitButton' })
defineProps<{ busy: boolean }>()
</script>
