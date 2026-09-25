package com.example.jms.model;

public class NotificationMessage {
    private String notificationId;
    private String recipient;
    private String content;

    public NotificationMessage() {
    }

    public NotificationMessage(String notificationId, String recipient, String content) {
        this.notificationId = notificationId;
        this.recipient = recipient;
        this.content = content;
    }

    public String getNotificationId() {
        return notificationId;
    }

    public void setNotificationId(String notificationId) {
        this.notificationId = notificationId;
    }

    public String getRecipient() {
        return recipient;
    }

    public void setRecipient(String recipient) {
        this.recipient = recipient;
    }

    public String getContent() {
        return content;
    }

    public void setContent(String content) {
        this.content = content;
    }

    @Override
    public String toString() {
        return "NotificationMessage{" +
                "notificationId='" + notificationId + '\'' +
                ", recipient='" + recipient + '\'' +
                ", content='" + content + '\'' +
                '}';
    }
}
