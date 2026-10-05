<script setup lang="ts">
defineProps<{ value: string | null, display: string, fullDisplay: string | null }>()
</script>

<template>
  <span v-if="value" :title="fullDisplay ?? undefined">{{ display }}</span>
</template>
