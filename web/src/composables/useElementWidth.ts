import { onBeforeUnmount, onMounted, ref, type Ref } from 'vue'

/** 观察容器实际宽度，供数据表按可用空间切换列与窄屏卡片。 */
export function useElementWidth(target: Ref<HTMLElement | null>) {
  const width = ref(0)
  let observer: ResizeObserver | undefined

  const measure = () => {
    if (target.value) width.value = target.value.clientWidth
  }

  onMounted(() => {
    measure()
    if (!target.value || typeof ResizeObserver === 'undefined') return
    observer = new ResizeObserver(measure)
    observer.observe(target.value)
  })

  onBeforeUnmount(() => observer?.disconnect())

  return width
}
