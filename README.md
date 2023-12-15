# Widecast

A scalable broadcast queue optimized for large numbers of subscribers.

It aims to have comparable performance to tokio's broadcast queue at low
susbcriber counts while having better performance and consistently low tail
latency at high numbers of subscribers (100 - 10k+).

