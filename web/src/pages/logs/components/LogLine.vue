<template>
  <div class="mt-log-line" :class="`is-${entry.level}`">
    <span class="mt-log-line__no">{{ entry.no }}</span>
    <span class="mt-log-line__time">{{ entry.time }}</span>
    <span class="mt-log-line__level" :class="`is-${entry.level}`">
      {{ entry.level.toUpperCase() }}
    </span>
    <span class="mt-log-line__msg">
      <span class="mt-log-line__target">{{ entry.target }}</span>
      {{ entry.message }}
    </span>
  </div>
</template>

<script setup lang="ts">
defineOptions({ name: 'LogLine' })

defineProps<{
  entry: {
    no: number
    time: string
    level: string
    target: string
    message: string
  }
}>()
</script>
