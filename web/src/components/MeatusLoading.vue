<template>
  <div
    class="mt-loading"
    :class="[`mt-loading--${size}`, { 'is-fullscreen': fullscreen }]"
    role="status"
    aria-live="polite"
    :aria-label="label"
    aria-busy="true"
  >
    <div class="mt-loading__core">
      <span class="mt-loading__halo" aria-hidden="true" />
      <span class="mt-loading__ring" aria-hidden="true" />
      <span class="mt-loading__mark">
        <img src="/brand/meatus-icon-64.png" alt="" width="40" height="40" aria-hidden="true" />
      </span>
    </div>

    <div class="mt-loading__wordmark">
      <b>MEATUS</b>
      <span>CONTROL PLANE</span>
    </div>

    <div class="mt-loading__label">
      <span>{{ label }}</span>
      <span class="mt-loading__dots" aria-hidden="true"><i /><i /><i /></span>
    </div>
  </div>
</template>

<script setup lang="ts">
defineOptions({ name: 'MeatusLoading' })

withDefaults(
  defineProps<{
    label?: string
    size?: 'sm' | 'md' | 'lg'
    fullscreen?: boolean
  }>(),
  {
    label: '正在加载',
    size: 'md',
    fullscreen: false,
  },
)
</script>

<style scoped>
.mt-loading {
  --mt-loading-mark: 54px;
  --mt-loading-core: 88px;
  --mt-loading-brand: 11px;
  --mt-loading-label: 12.5px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 0;
  width: 100%;
  min-height: 220px;
  padding: 32px 24px;
  color: var(--ink);
  text-align: center;
}

.mt-loading--sm {
  --mt-loading-mark: 36px;
  --mt-loading-core: 60px;
  --mt-loading-brand: 9px;
  --mt-loading-label: 11px;
  min-height: 140px;
  padding: 20px 16px;
}

.mt-loading--lg {
  --mt-loading-mark: 66px;
  --mt-loading-core: 108px;
  --mt-loading-brand: 12px;
  --mt-loading-label: 13px;
}

.mt-loading.is-fullscreen {
  position: fixed;
  inset: 0;
  z-index: 120;
  min-height: 100dvh;
  padding: 32px;
  background:
    radial-gradient(55% 48% at 50% 42%, rgba(255, 255, 255, 0.62), transparent 72%),
    radial-gradient(42% 36% at 82% 12%, rgba(130, 221, 45, 0.06), transparent 70%),
    var(--stage);
}

.mt-loading__core {
  position: relative;
  display: grid;
  place-items: center;
  width: var(--mt-loading-core);
  height: var(--mt-loading-core);
  flex: none;
}

.mt-loading__core::before {
  content: '';
  position: absolute;
  inset: 5px;
  border: 1px solid rgba(46, 58, 62, 0.1);
  border-radius: 50%;
  background: rgba(255, 255, 255, 0.18);
}

.mt-loading__halo {
  position: absolute;
  inset: 2px;
  border: 1px solid rgba(43, 196, 220, 0.28);
  border-radius: 50%;
  animation: mt-loading-halo 2.2s ease-out infinite;
}

.mt-loading__ring {
  position: absolute;
  inset: 0;
  border-radius: 50%;
  background: conic-gradient(
    from 0deg,
    rgba(43, 196, 220, 0) 0deg,
    rgba(43, 196, 220, 0.08) 150deg,
    rgba(43, 196, 220, 0.5) 250deg,
    #2bc4dc 305deg,
    rgba(35, 109, 233, 0.62) 338deg,
    rgba(35, 109, 233, 0) 360deg
  );
  -webkit-mask: radial-gradient(farthest-side, transparent calc(100% - 2.5px), #000 calc(100% - 1.5px));
  mask: radial-gradient(farthest-side, transparent calc(100% - 2.5px), #000 calc(100% - 1.5px));
  animation: mt-loading-spin 1.45s linear infinite;
}

.mt-loading__mark {
  position: relative;
  display: grid;
  place-items: center;
  width: var(--mt-loading-mark);
  height: var(--mt-loading-mark);
  padding: 7px;
  border: 1px solid rgba(46, 58, 62, 0.1);
  border-radius: 16px;
  background: #fff;
  box-shadow:
    inset 0 1px 0 rgba(255, 255, 255, 0.9),
    0 1px 2px rgba(16, 30, 40, 0.05),
    0 12px 28px rgba(16, 30, 40, 0.1);
  animation: mt-loading-breathe 2.4s ease-in-out infinite;
}

.mt-loading__mark img {
  display: block;
  width: 100%;
  height: 100%;
  object-fit: contain;
}

.mt-loading__wordmark {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 3px;
  margin-top: 18px;
}

.mt-loading__wordmark b {
  font-size: var(--mt-loading-brand);
  font-weight: 760;
  letter-spacing: 0.22em;
  color: var(--ink);
  text-indent: 0.22em;
}

.mt-loading__wordmark span {
  font-size: 8.5px;
  font-weight: 650;
  letter-spacing: 0.17em;
  text-indent: 0.17em;
  color: var(--ink-soft);
}

.mt-loading__label {
  display: inline-flex;
  align-items: baseline;
  gap: 5px;
  margin-top: 10px;
  font-size: var(--mt-loading-label);
  color: var(--ink-muted);
}

.mt-loading__dots {
  display: inline-flex;
  gap: 3px;
  transform: translateY(-1px);
}

.mt-loading__dots i {
  width: 3.5px;
  height: 3.5px;
  border-radius: 50%;
  background: var(--mt-accent);
  opacity: 0.3;
  animation: mt-loading-dot 1.45s ease-in-out infinite;
}

.mt-loading__dots i:nth-child(2) {
  animation-delay: 0.18s;
}

.mt-loading__dots i:nth-child(3) {
  animation-delay: 0.36s;
}

@keyframes mt-loading-spin {
  to {
    transform: rotate(360deg);
  }
}

@keyframes mt-loading-halo {
  0% {
    opacity: 0.55;
    transform: scale(0.96);
  }
  72% {
    opacity: 0;
    transform: scale(1.16);
  }
  100% {
    opacity: 0;
    transform: scale(1.16);
  }
}

@keyframes mt-loading-breathe {
  0%,
  100% {
    transform: scale(1);
  }
  50% {
    transform: scale(1.035);
  }
}

@keyframes mt-loading-dot {
  0%,
  80%,
  100% {
    opacity: 0.22;
    transform: translateY(0);
  }
  40% {
    opacity: 1;
    transform: translateY(-3px);
  }
}

@media (prefers-reduced-motion: reduce) {
  .mt-loading__ring,
  .mt-loading__halo,
  .mt-loading__mark,
  .mt-loading__dots i {
    animation: none;
  }

  .mt-loading__dots i {
    opacity: 0.6;
  }
}
</style>
