<script setup lang="ts">
import BaseCard from '@/components/base/BaseCard.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'

const enabled = defineModel<boolean>({ required: true })
</script>

<template>
  <BaseCard title="阻断降智对话" description="打开后，上游返回 312 字节 turn-state 时不把该响应发给客户端">
    <BaseSwitch v-model="enabled" class="justify-self-start" label="阻断降智对话" show-label />
  </BaseCard>
</template>
