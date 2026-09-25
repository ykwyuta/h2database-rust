package com.example.jms.model;

public class QueueMessage {
    private Long offset;
    private String timestamp;
    private String msgId;
    private String payload;

    public QueueMessage() {}

    public QueueMessage(Long offset, String timestamp, String msgId, String payload) {
        this.offset = offset;
        this.timestamp = timestamp;
        this.msgId = msgId;
        this.payload = payload;
    }

    public Long getOffset() { return offset; }
    public void setOffset(Long offset) { this.offset = offset; }

    public String getTimestamp() { return timestamp; }
    public void setTimestamp(String timestamp) { this.timestamp = timestamp; }

    public String getMsgId() { return msgId; }
    public void setMsgId(String msgId) { this.msgId = msgId; }

    public String getPayload() { return payload; }
    public void setPayload(String payload) { this.payload = payload; }

    @Override
    public String toString() {
        return "QueueMessage{" +
                "offset=" + offset +
                ", timestamp=" + timestamp +
                ", msgId='" + msgId + '\'' +
                ", payload='" + payload + '\'' +
                '}';
    }
}
