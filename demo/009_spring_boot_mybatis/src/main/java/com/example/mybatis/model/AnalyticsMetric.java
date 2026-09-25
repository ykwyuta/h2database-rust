package com.example.mybatis.model;

public class AnalyticsMetric {
    private String metricName;
    private Double rawVal;
    private Double lnVal;
    private Double expVal;
    private Double truncVal;
    private String initcapStr;
    private String replacedStr;
    private Boolean regexMatched;

    public AnalyticsMetric() {}

    public String getMetricName() { return metricName; }
    public void setMetricName(String metricName) { this.metricName = metricName; }

    public Double getRawVal() { return rawVal; }
    public void setRawVal(Double rawVal) { this.rawVal = rawVal; }

    public Double getLnVal() { return lnVal; }
    public void setLnVal(Double lnVal) { this.lnVal = lnVal; }

    public Double getExpVal() { return expVal; }
    public void setExpVal(Double expVal) { this.expVal = expVal; }

    public Double getTruncVal() { return truncVal; }
    public void setTruncVal(Double truncVal) { this.truncVal = truncVal; }

    public String getInitcapStr() { return initcapStr; }
    public void setInitcapStr(String initcapStr) { this.initcapStr = initcapStr; }

    public String getReplacedStr() { return replacedStr; }
    public void setReplacedStr(String replacedStr) { this.replacedStr = replacedStr; }

    public Boolean getRegexMatched() { return regexMatched; }
    public void setRegexMatched(Boolean regexMatched) { this.regexMatched = regexMatched; }

    @Override
    public String toString() {
        return "AnalyticsMetric{" +
                "metricName='" + metricName + '\'' +
                ", rawVal=" + rawVal +
                ", lnVal=" + lnVal +
                ", expVal=" + expVal +
                ", truncVal=" + truncVal +
                ", initcapStr='" + initcapStr + '\'' +
                ", replacedStr='" + replacedStr + '\'' +
                ", regexMatched=" + regexMatched +
                '}';
    }
}
