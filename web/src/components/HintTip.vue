<template>
  <SlTooltip
    :content="content"
    :placement="placement"
    size="sm"
    :delay="120"
    :close-delay="40"
    :offset="6"
  >
    <component
      :is="focusable ? 'button' : 'span'"
      :type="focusable ? 'button' : undefined"
      class="mt-hint-tip"
      :tabindex="focusable ? 0 : -1"
      :role="focusable ? undefined : 'button'"
      :aria-label="`提示：${content}`"
      @click.prevent.stop
      @pointerdown.stop
    >
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" aria-hidden="true">
        <circle cx="12" cy="12" r="8.4" stroke-width="1.35" />
        <path d="M12 11.2v5.2" stroke-width="1.5" stroke-linecap="round" />
        <circle cx="12" cy="7.9" r="0.9" fill="currentColor" stroke="none" />
      </svg>
    </component>
  </SlTooltip>
</template>

<script setup lang="ts">
import { SlTooltip } from '@sailonix/ui/tooltip'

withDefaults(
  defineProps<{
    content: string
    placement?: 'top' | 'bottom' | 'left' | 'right'
    focusable?: boolean
  }>(),
  { placement: 'top', focusable: true },
)
</script>

<style scoped>
.mt-hint-tip {
  display: inline-flex;
  flex: none;
  align-items: center;
  justify-content: center;
  width: 16px;
  height: 16px;
  margin-left: 2px;
  padding: 0;
  border: 0;
  border-radius: 50%;
  background: transparent;
  color: var(--ink-faint);
  line-height: 0;
  vertical-align: middle;
  cursor: help;
  transform: translateY(1px);
  transition: color 0.16s ease, background-color 0.16s ease;
}

.mt-hint-tip:hover,
.mt-hint-tip:focus-visible {
  color: var(--blue);
  background: var(--blue-soft);
}

.mt-hint-tip:focus-visible {
  outline: 2px solid rgba(35, 109, 233, 0.32);
  outline-offset: 1px;
}

.mt-hint-tip svg {
  width: 13px;
  height: 13px;
}
</style>
