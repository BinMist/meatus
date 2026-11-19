<template>
  <div class="mt-login-field">
    <div class="mt-login-field-label">
      <label for="mtLoginToken">ADMIN TOKEN</label>
      <span>REQUIRED / 01</span>
    </div>
    <sl-input
      id="mtLoginToken"
      :value="modelValue"
      class="mt-login-input"
      :type="showToken ? 'text' : 'password'"
      placeholder="输入 admin_token"
      autocomplete="current-password"
      autocapitalize="off"
      spellcheck="false"
      :disabled="disabled"
      :invalid="invalid"
      :aria-describedby="describedBy"
      :aria-invalid="invalid ? 'true' : undefined"
      @update:value="emit('update:modelValue', $event)"
    >
      <template #prefix>
        <span class="mt-login-control-lead" aria-hidden="true">
          <svg viewBox="0 0 256 256" fill="currentColor"><path d="M208,80H176V56a48,48,0,0,0-96,0V80H48A16,16,0,0,0,32,96V208a16,16,0,0,0,16,16H208a16,16,0,0,0,16-16V96A16,16,0,0,0,208,80ZM96,56a32,32,0,0,1,64,0V80H96ZM208,208H48V96H208V208Zm-68-56a12,12,0,1,1-12-12A12,12,0,0,1,140,152Z" /></svg>
        </span>
      </template>
      <template #suffix>
        <sl-icon-button
          class="mt-login-reveal"
          type="ghost"
          size="sm"
          :aria-label="showToken ? '隐藏令牌' : '显示令牌'"
          :title="showToken ? '隐藏令牌' : '显示令牌'"
          @click="showToken = !showToken"
        >
          <svg v-if="!showToken" viewBox="0 0 24 24" aria-hidden="true">
            <path
              d="M2.036 12.322a1.012 1.012 0 0 1 0-.644C3.399 7.254 7.536 4.5 12 4.5c4.464 0 8.601 2.754 9.964 7.178a1.01 1.01 0 0 1 0 .644C20.601 16.746 16.464 19.5 12 19.5c-4.464 0-8.601-2.754-9.964-7.178Z"
              fill="none"
              stroke="currentColor"
              stroke-width="1.7"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
            <path
              d="M15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z"
              fill="none"
              stroke="currentColor"
              stroke-width="1.7"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
          <svg v-else viewBox="0 0 24 24" aria-hidden="true">
            <path
              d="M3 3l18 18M10.584 10.587A3 3 0 0 0 12 15a3 3 0 0 0 2.414-4.413M9.363 5.365A10.477 10.477 0 0 1 12 4.5c4.464 0 8.601 2.754 9.964 7.178a1.01 1.01 0 0 1 0 .644 10.523 10.523 0 0 1-4.293 5.528M6.228 6.228A10.45 10.45 0 0 0 2.036 11.678a1.012 1.012 0 0 0 0 .644C3.399 16.746 7.536 19.5 12 19.5c1.006 0 1.978-.142 2.9-.405"
              fill="none"
              stroke="currentColor"
              stroke-width="1.7"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </sl-icon-button>
      </template>
    </sl-input>
    <div class="mt-login-field-hint" id="mtLoginTokenHint">
      内容不会写入 localStorage；仅换取 HttpOnly 会话。
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { SlIconButton } from '@sailonix/ui/icon-button'
import '@sailonix/ui/icon-button/style'
import { SlInput } from '@sailonix/ui/input'

defineOptions({ name: 'LoginTokenField' })

withDefaults(
  defineProps<{
    modelValue: string
    disabled?: boolean
    invalid?: boolean
    describedBy?: string
  }>(),
  {
    disabled: false,
    invalid: false,
    describedBy: 'mtLoginTokenHint',
  },
)

const emit = defineEmits<{ 'update:modelValue': [value: string] }>()
const showToken = ref(false)
</script>
