package com.example.mybatis.model;

import java.time.LocalDateTime;

public class Subscription {
    private Integer id;
    private Long userId;
    private String planName;
    private String startTime;
    private String expireTime;

    public Subscription() {}

    public Subscription(Integer id, Long userId, String planName, String startTime, String expireTime) {
        this.id = id;
        this.userId = userId;
        this.planName = planName;
        this.startTime = startTime;
        this.expireTime = expireTime;
    }

    public Integer getId() { return id; }
    public void setId(Integer id) { this.id = id; }

    public Long getUserId() { return userId; }
    public void setUserId(Long userId) { this.userId = userId; }

    public String getPlanName() { return planName; }
    public void setPlanName(String planName) { this.planName = planName; }

    public String getStartTime() { return startTime; }
    public void setStartTime(String startTime) { this.startTime = startTime; }

    public String getExpireTime() { return expireTime; }
    public void setExpireTime(String expireTime) { this.expireTime = expireTime; }

    @Override
    public String toString() {
        return "Subscription{" +
                "id=" + id +
                ", userId=" + userId +
                ", planName='" + planName + '\'' +
                ", startTime=" + startTime +
                ", expireTime=" + expireTime +
                '}';
    }
}
