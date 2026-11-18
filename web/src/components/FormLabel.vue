<template>
  <span>{{ label }}</span>
  <span v-if="required" class="sl-form-item__required" aria-hidden="true">*</span>
  <HintTip v-if="hint" :content="hint" />
</template>

<script setup lang="ts">
import HintTip from './HintTip.vue'

withDefaults(
  defineProps<{
    label: string
    required?: boolean
    hint?: string
  }>(),
  { required: false, hint: '' },
)
</script>
